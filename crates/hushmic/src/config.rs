use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub mic: Option<String>, // real source node.name; None = use system default
    pub model: String,       // model file stem under /usr/share/hushmic/models
    pub attn_limit: f32,     // dB cap for the plugin control port
    pub set_default: bool,   // make hushmic the default input on enable
    pub autostart: bool,     // launch on login
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
        if !self.attn_limit.is_finite() {
            self.attn_limit = 100.0;
        }
        self.attn_limit = self.attn_limit.clamp(0.0, 100.0);
    }
    pub fn save(&self) -> std::io::Result<()> {
        // atomic_write adds the fsync the old temp+rename here lacked:
        // without it the rename can outlive a crash that the data doesn't.
        let s = toml::to_string_pretty(self).expect("serialize config");
        crate::fsutil::atomic_write(&Self::path(), s.as_bytes())
    }
}
