use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::{fs, path::PathBuf};

/// One microphone's remembered settings (roadmap item 5). Keyed by
/// `node.name` in [`Config::mic_prefs`]; the globals stay as the
/// System-default settings and the fallback for mics without an entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicPrefs {
    pub model: String,
    pub attn_limit: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub mic: Option<String>, // real source node.name; None = use system default
    pub model: String,       // model file stem under /usr/share/hushmic/models
    pub attn_limit: f32,     // dB cap for the plugin control port
    pub set_default: bool,   // make hushmic the default input on enable
    pub autostart: bool,     // launch on login
    /// Per-microphone settings, keyed by node.name. Never serialized while
    /// empty, so configs that predate (or never use) the feature stay
    /// byte-identical.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mic_prefs: BTreeMap<String, MicPrefs>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            mic: None,
            model: "dpdfnet8_48khz_hr".into(),
            attn_limit: 100.0,
            // Opt-in, matching the documented flow: creating the virtual mic is
            // additive, but repointing the SYSTEM default input is invasive and
            // must be the user's explicit choice (README: "flip Set as default").
            set_default: false,
            autostart: false,
            mic_prefs: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        ProjectDirs::from("io", "hushmic", "hushmic")
            .expect("home dir")
            .config_dir()
            .join("config.toml")
    }
    pub fn load() -> Self {
        let p = Self::path();
        let mut cfg = match fs::read_to_string(&p) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => c,
                Err(e) => {
                    // Don't silently replace a hand-edited file with defaults:
                    // keep the evidence aside and say so, because the defaults
                    // (enabled=true) have real side effects on next launch.
                    eprintln!(
                        "hushmic: {} is invalid ({e}); moving it to config.toml.bad \
                         and starting from defaults",
                        p.display()
                    );
                    let _ = fs::rename(&p, p.with_extension("toml.bad"));
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        };
        cfg.sanitize();
        cfg
    }

    /// Clamp hand-editable values to what the rest of the app can represent: a
    /// non-finite attn_limit would be rendered as a literal `NaN` token in the
    /// filter-chain conf (which PipeWire rejects), and the plugin's control
    /// port is bounded 0..=100.
    pub fn sanitize(&mut self) {
        let clamp = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 100.0)
            } else {
                100.0
            }
        };
        self.attn_limit = clamp(self.attn_limit);
        // Per-mic entries are just as hand-editable as the globals. A typo'd
        // model id is left alone deliberately — enable()'s asset preflight
        // reports it with the missing .onnx path, same as the global one.
        for p in self.mic_prefs.values_mut() {
            p.attn_limit = clamp(p.attn_limit);
        }
    }
    /// Select a mic (or System default): sets `mic`, and when the pick has
    /// a saved profile, loads it into the tray-visible `model`/`attn_limit`.
    /// A pick WITHOUT a profile keeps the current values and creates no
    /// entry (entries appear only when settings are changed under a mic).
    pub fn apply_mic_selection(&mut self, pick: Option<String>) {
        if let Some(p) = pick.as_deref().and_then(|m| self.mic_prefs.get(m)) {
            self.model = p.model.clone();
            self.attn_limit = p.attn_limit;
        }
        self.mic = pick;
    }

    /// After a `model`/`attn_limit` change: upsert the selected mic's
    /// profile. No-op in System-default mode — globals are that profile.
    pub fn remember_selected_prefs(&mut self) {
        if let Some(m) = &self.mic {
            self.mic_prefs.insert(
                m.clone(),
                MicPrefs {
                    model: self.model.clone(),
                    attn_limit: self.attn_limit,
                },
            );
        }
    }

    /// The (model, attn_limit) the chain should run with, following the
    /// ACTIVE device: the pinned mic's profile; in follow-default mode the
    /// current default source's profile if one exists; otherwise the
    /// globals. The only place profile lookup happens.
    pub fn effective_settings(
        &self,
        effective_mic: Option<&str>,
        default_source: Option<&str>,
    ) -> (String, f32) {
        let profile = match effective_mic {
            // Pinned: the mic's own profile or the globals — never the
            // default source's (that device is not in use).
            Some(m) => self.mic_prefs.get(m),
            // Follow-default (incl. recovery fallback): the active device
            // IS the default source; honor its profile when known.
            None => default_source.and_then(|d| self.mic_prefs.get(d)),
        };
        match profile {
            Some(p) => (p.model.clone(), p.attn_limit),
            None => (self.model.clone(), self.attn_limit),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        // atomic_write adds the fsync the old temp+rename here lacked:
        // without it the rename can outlive a crash that the data doesn't.
        let s = toml::to_string_pretty(self).expect("serialize config");
        crate::fsutil::atomic_write(&Self::path(), s.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_entry(name: &str, model: &str, attn: f32) -> Config {
        let mut c = Config::default();
        c.mic_prefs.insert(
            name.into(),
            MicPrefs {
                model: model.into(),
                attn_limit: attn,
            },
        );
        c
    }

    #[test]
    fn prefs_roundtrip_and_stay_absent_when_unused() {
        // Never used the feature: the serialized file must not mention it.
        let plain = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!plain.contains("mic_prefs"), "{plain}");
        // With an entry: round-trips intact.
        let c = with_entry("alsa_input.rode", "dpdfnet2_48khz_hr", 24.0);
        let s = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.mic_prefs, c.mic_prefs);
    }

    #[test]
    fn old_config_without_the_table_loads() {
        let old = "enabled = true\nattn_limit = 87.0\n";
        let c: Config = toml::from_str(old).unwrap();
        assert!(c.mic_prefs.is_empty());
        assert_eq!(c.attn_limit, 87.0);
    }

    #[test]
    fn sanitize_clamps_entry_attn_like_the_global() {
        let mut c = with_entry("m", "dpdfnet8_48khz_hr", 250.0);
        c.mic_prefs.insert(
            "n".into(),
            MicPrefs {
                model: "dpdfnet8_48khz_hr".into(),
                attn_limit: f32::NAN,
            },
        );
        c.sanitize();
        assert_eq!(c.mic_prefs["m"].attn_limit, 100.0);
        assert_eq!(c.mic_prefs["n"].attn_limit, 100.0);
    }

    #[test]
    fn selection_loads_the_profile_and_keeps_values_otherwise() {
        let mut c = with_entry("rode", "dpdfnet2_48khz_hr", 24.0);
        c.model = "dpdfnet8_48khz_hr".into();
        c.attn_limit = 100.0;
        c.apply_mic_selection(Some("rode".into()));
        assert_eq!(c.mic.as_deref(), Some("rode"));
        assert_eq!(c.model, "dpdfnet2_48khz_hr");
        assert_eq!(c.attn_limit, 24.0);
        // A mic without a profile: values carry over, no phantom entry.
        c.apply_mic_selection(Some("webcam".into()));
        assert_eq!(c.model, "dpdfnet2_48khz_hr");
        assert_eq!(c.attn_limit, 24.0);
        assert!(!c.mic_prefs.contains_key("webcam"));
        // Back to System default: globals untouched by the switch itself.
        c.apply_mic_selection(None);
        assert_eq!(c.mic, None);
        assert_eq!(c.attn_limit, 24.0);
    }

    #[test]
    fn changes_upsert_only_under_a_pinned_mic() {
        let mut c = Config {
            mic: Some("rode".into()),
            attn_limit: 24.0,
            ..Config::default()
        };
        c.remember_selected_prefs();
        assert_eq!(c.mic_prefs["rode"].attn_limit, 24.0);
        c.attn_limit = 12.0;
        c.remember_selected_prefs();
        assert_eq!(c.mic_prefs["rode"].attn_limit, 12.0);
        // System-default mode: globals ARE the profile — no entry appears.
        c.mic = None;
        c.attn_limit = 6.0;
        c.remember_selected_prefs();
        assert_eq!(c.mic_prefs.len(), 1);
        assert_eq!(c.mic_prefs["rode"].attn_limit, 12.0);
    }

    #[test]
    fn effective_settings_follow_the_active_device() {
        let mut c = with_entry("rode", "dpdfnet2_48khz_hr", 24.0);
        c.mic_prefs.insert(
            "builtin".into(),
            MicPrefs {
                model: "dpdfnet8_48khz_hr".into(),
                attn_limit: 6.0,
            },
        );
        c.model = "dpdfnet8_48khz_hr".into();
        c.attn_limit = 100.0;
        // Pinned with a profile.
        assert_eq!(
            c.effective_settings(Some("rode"), Some("builtin")),
            ("dpdfnet2_48khz_hr".into(), 24.0)
        );
        // Pinned without a profile: globals (NOT the default's profile).
        assert_eq!(
            c.effective_settings(Some("webcam"), Some("builtin")),
            ("dpdfnet8_48khz_hr".into(), 100.0)
        );
        // Follow-default with the default's profile (recovery fallback).
        assert_eq!(
            c.effective_settings(None, Some("builtin")),
            ("dpdfnet8_48khz_hr".into(), 6.0)
        );
        // Follow-default without a profile / unknown default: globals.
        assert_eq!(
            c.effective_settings(None, Some("other")),
            ("dpdfnet8_48khz_hr".into(), 100.0)
        );
        assert_eq!(
            c.effective_settings(None, None),
            ("dpdfnet8_48khz_hr".into(), 100.0)
        );
    }
}
