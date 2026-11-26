use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::{fs, path::PathBuf};

/// Which tray icon set HushMic asks the desktop for (issue #17). The
/// desktop does the drawing either way: `Color` names the shipped coloured
/// ladder, `Symbolic` the monochrome one that the theme recolors, and
/// `Auto` picks between them from the running desktop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrayIcon {
    #[default]
    Auto,
    Color,
    Symbolic,
}

impl TrayIcon {
    /// The three words the file and the CLI accept, in listing order.
    pub const VALUES: [&'static str; 3] = ["auto", "color", "symbolic"];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "color" => Some(Self::Color),
            "symbolic" => Some(Self::Symbolic),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Color => "color",
            Self::Symbolic => "symbolic",
        }
    }

    fn is_default(&self) -> bool {
        *self == Self::Auto
    }
}

impl<'de> Deserialize<'de> for TrayIcon {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // A typo in a hand-edited file must cost the reader nothing but
        // this one field: the strict derive would fail the whole document,
        // and load() reacts to that by moving the file aside and starting
        // from defaults (enabled = true has real side effects). Read any
        // value, keep the ones we know, fall back to auto for the rest.
        let raw = toml::Value::deserialize(d)?;
        Ok(raw.as_str().and_then(Self::parse).unwrap_or_default())
    }
}

/// Which inference engine the filter chain may use (issue #18). `Auto`
/// lets the plugin pick the native engine where the CPU and the installed
/// weights allow it, ONNX otherwise; `Onnx` keeps it on ONNX Runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Inference {
    #[default]
    Auto,
    Onnx,
}

impl Inference {
    /// The words the file and the CLI accept, in listing order.
    pub const VALUES: [&'static str; 2] = ["auto", "onnx"];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "onnx" => Some(Self::Onnx),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Onnx => "onnx",
        }
    }

    fn is_default(&self) -> bool {
        *self == Self::Auto
    }
}

impl<'de> Deserialize<'de> for Inference {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Lenient like TrayIcon: a typo costs this field, not the file.
        let raw = toml::Value::deserialize(d)?;
        Ok(raw.as_str().and_then(Self::parse).unwrap_or_default())
    }
}

/// The window's colours (v1 app): follow the desktop's light/dark setting,
/// or pin one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Dark,
    Light,
}

impl Appearance {
    /// The words the file and the CLI accept, in listing order.
    pub const VALUES: [&'static str; 3] = ["system", "dark", "light"];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Self::System),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    fn is_default(&self) -> bool {
        *self == Self::System
    }
}

impl<'de> Deserialize<'de> for Appearance {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Lenient like TrayIcon: a typo costs this field, not the file.
        let raw = toml::Value::deserialize(d)?;
        Ok(raw.as_str().and_then(Self::parse).unwrap_or_default())
    }
}

/// An app whose "using your microphone directly" notice the user hid
/// ("Don't show again for {app}"). `app` is the key the detection reports
/// (Flatpak app id, binary or name); `name` is what Settings shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiddenNotice {
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One microphone's remembered settings. Keyed by `node.name` in
/// [`Config::mic_prefs`]; the globals are the settings for every device
/// without an entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicPrefs {
    pub model: String,
    pub attn_limit: f32,
}

/// Whose model and strength are in effect: the device they belong to and
/// the values. Built by [`Config::profile_for`] / [`Config::active_profile`].
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveProfile {
    /// The device in use (node.name); None when no device can own a
    /// profile (default source unknown, or our own node). Where edits land
    /// is [`Config::edit_target`].
    pub device: Option<String>,
    /// `device` has a saved entry; false = it runs on the globals.
    pub saved: bool,
    pub model: String,
    pub attn_limit: f32,
}

impl ActiveProfile {
    /// The log's description, e.g. `RODE NT-USB profile
    /// (dpdfnet8, 100 dB)`; `name` is the device's display name.
    pub fn describe(&self, name: Option<&str>) -> String {
        let short = self.model.split('_').next().unwrap_or(&self.model);
        let whose = match (name.or(self.device.as_deref()), self.saved) {
            (Some(n), true) => format!("{n} profile"),
            (Some(n), false) => format!("defaults for {n}"),
            (None, _) => "defaults".to_string(),
        };
        format!("{whose} ({short}, {} dB)", self.attn_limit)
    }
}

/// Whether a source may own a per-mic profile: a real capture device, never
/// one of HushMic's own nodes (with "Set as default microphone" on, the
/// default source IS our output) nor a monitor.
pub fn can_own_profile(name: &str) -> bool {
    !name.is_empty() && !name.starts_with("hushmic_") && !name.ends_with(".monitor")
}

/// The device whose settings apply: the pinned mic the chain runs on, else
/// the default source it follows. Pinned wins even without a profile — the
/// default source is not in use then.
pub fn profile_owner<'a>(
    effective_mic: Option<&'a str>,
    followed_default: Option<&'a str>,
) -> Option<&'a str> {
    effective_mic
        .or(followed_default)
        .filter(|d| can_own_profile(d))
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
    /// The user has been through the portal's shortcut-binding dialog
    /// once (the compositor owns the actual key assignments — we persist
    /// no key names, only whether to silently re-register at startup).
    /// Skipped while false so pre-feature configs stay byte-identical.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub shortcuts_setup: bool,
    /// The first close of the window told the user that HushMic keeps
    /// running (once, ever). Skipped while false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub close_notice_sent: bool,
    /// Register a tray icon (`--headless` overrides this to false for one
    /// run). Skipped while true so existing files stay byte-identical.
    #[serde(skip_serializing_if = "is_true")]
    pub tray: bool,
    /// Desktop notifications (failures, mic-test progress, recovery).
    #[serde(skip_serializing_if = "is_true")]
    pub notifications: bool,
    /// Which tray icon set the SNI host is asked for. Skipped while `auto`
    /// so existing files stay byte-identical.
    #[serde(skip_serializing_if = "TrayIcon::is_default")]
    pub tray_icon: TrayIcon,
    /// The inference engine choice handed to the plugin. Skipped while
    /// `auto` so existing files stay byte-identical.
    #[serde(skip_serializing_if = "Inference::is_default")]
    pub inference: Inference,
    /// Keep to the talker's voice: voices farther from the microphone stay
    /// out, also after a pause (the plugin's voice hold, issue #34). Skipped
    /// while false so existing files stay byte-identical.
    #[serde(skip_serializing_if = "is_false")]
    pub voice_hold: bool,
    /// The window's colours. Skipped while `system` so existing files stay
    /// byte-identical.
    #[serde(skip_serializing_if = "Appearance::is_default")]
    pub appearance: Appearance,
    /// Direct-mic notices hidden per app. Skipped while empty.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_notices"
    )]
    pub hidden_notices: Vec<HiddenNotice>,
    /// Where cleaned files go when they cannot go next to the original
    /// (inside a sandbox): chosen once through the portal. Skipped while
    /// unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_folder: Option<std::path::PathBuf>,
    /// The Files page's strength (a dB cap) the user last chose: files
    /// added after a restart start at it. Skipped while unset (Strong).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_strength: Option<f32>,
    /// The speakers cleaner (`[speakers]`): absent until it is first used,
    /// so existing files stay byte-identical. A build without it reads the
    /// section and writes it back as it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speakers: Option<SpeakersConfig>,
}

/// The speakers cleaner's saved settings: a virtual output "HushMic
/// Speakers" whose sound reaches a real output cleaned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeakersConfig {
    /// Whether the cleaner runs. Off until the user turns it on.
    pub enabled: bool,
    /// The real output it plays to (node.name); None = the system default
    /// (looking past HushMic Speakers itself).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The strength (a dB cap), one of the four steps from the UI.
    pub attn_limit: f32,
}

impl Default for SpeakersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            output: None,
            attn_limit: 100.0,
        }
    }
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Read `hidden_notices` entry by entry: a hand-edited entry without an
/// app (or of the wrong type) is dropped, the rest of the file stays.
fn lenient_notices<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<HiddenNotice>, D::Error> {
    let raw = toml::Value::deserialize(d)?;
    let entries = raw.as_array().map(Vec::as_slice).unwrap_or_default();
    Ok(entries
        .iter()
        .filter_map(|v| {
            let t = v.as_table()?;
            let app = t.get("app")?.as_str()?.trim();
            (!app.is_empty()).then(|| HiddenNotice {
                app: app.to_string(),
                name: t.get("name").and_then(|n| n.as_str()).map(str::to_string),
            })
        })
        .collect())
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
            // must be the user's explicit choice ("Use as default microphone").
            set_default: false,
            autostart: false,
            mic_prefs: BTreeMap::new(),
            shortcuts_setup: false,
            close_notice_sent: false,
            tray: true,
            notifications: true,
            tray_icon: TrayIcon::Auto,
            inference: Inference::Auto,
            voice_hold: false,
            appearance: Appearance::System,
            hidden_notices: Vec::new(),
            files_folder: None,
            files_strength: None,
            speakers: None,
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
        #[cfg(feature = "speakers")]
        if let Some(s) = &mut self.speakers {
            s.attn_limit = clamp(s.attn_limit);
            if s.output.as_deref().is_some_and(|o| o.trim().is_empty()) {
                s.output = None;
            }
        }
    }

    /// The speakers cleaner's settings (the defaults while the config has
    /// no `[speakers]` section).
    #[cfg(feature = "speakers")]
    pub fn speakers(&self) -> SpeakersConfig {
        self.speakers.clone().unwrap_or_default()
    }

    /// Change the speakers settings; the section appears on the first
    /// change.
    #[cfg(feature = "speakers")]
    pub fn speakers_mut(&mut self) -> &mut SpeakersConfig {
        self.speakers.get_or_insert_with(SpeakersConfig::default)
    }
    /// Select a mic (or System default). Only `mic` changes: the pick's
    /// saved profile (or the defaults) applies through [`Self::profile_for`]
    /// once the chain restarts on it, so the defaults for devices without a
    /// profile never pick up another device's values.
    pub fn apply_mic_selection(&mut self, pick: Option<String>) {
        self.mic = pick;
    }

    /// The settings `device` runs with: its saved profile, or the globals
    /// when it has none (or when no device can own a profile at all).
    pub fn profile_for(&self, device: Option<&str>) -> ActiveProfile {
        let device = device.filter(|d| can_own_profile(d));
        match device.and_then(|d| self.mic_prefs.get(d)) {
            Some(p) => ActiveProfile {
                device: device.map(str::to_string),
                saved: true,
                model: p.model.clone(),
                attn_limit: p.attn_limit,
            },
            None => ActiveProfile {
                device: device.map(str::to_string),
                saved: false,
                model: self.model.clone(),
                attn_limit: self.attn_limit,
            },
        }
    }

    /// The profile in effect for a chain on `effective_mic` (None = it
    /// follows the default source, which is `followed_default`). The one
    /// place that decides whose settings apply — the chain, the tray,
    /// `status` and `--doctor` all come through here.
    pub fn active_profile(
        &self,
        effective_mic: Option<&str>,
        followed_default: Option<&str>,
    ) -> ActiveProfile {
        self.profile_for(profile_owner(effective_mic, followed_default))
    }

    /// Where a model/strength edit for the device in use lands: that
    /// device's profile, created on the first edit (seeded from the
    /// defaults), also when HushMic follows the system default onto it (UX
    /// panel B2: the strength is remembered for each microphone). None =
    /// the defaults, when no device can own a profile (none known, or our
    /// own node); every device without a profile runs with them.
    pub fn edit_target<'a>(&self, device: Option<&'a str>) -> Option<&'a str> {
        device.filter(|d| can_own_profile(d))
    }

    /// A model change from the tray or `config set`, for the device in
    /// use; see [`Self::edit_target`].
    pub fn set_model_for(&mut self, device: Option<&str>, model: String) {
        *self.settings_mut(device).0 = model;
    }

    /// A strength change; same routing as [`Self::set_model_for`].
    pub fn set_attn_for(&mut self, device: Option<&str>, attn_limit: f32) {
        *self.settings_mut(device).1 = attn_limit;
    }

    fn settings_mut(&mut self, device: Option<&str>) -> (&mut String, &mut f32) {
        match self.edit_target(device) {
            Some(d) => {
                let seed = MicPrefs {
                    model: self.model.clone(),
                    attn_limit: self.attn_limit,
                };
                let p = self.mic_prefs.entry(d.to_string()).or_insert(seed);
                (&mut p.model, &mut p.attn_limit)
            }
            None => (&mut self.model, &mut self.attn_limit),
        }
    }

    /// Writes config.toml, unless it already holds exactly these bytes
    /// (a mode change, which isn't stored, then costs no fsync).
    pub fn save(&self) -> std::io::Result<()> {
        write_if_changed(&Self::path(), &self.bytes()).map(|_| ())
    }

    fn bytes(&self) -> Vec<u8> {
        toml::to_string_pretty(self)
            .expect("serialize config")
            .into_bytes()
    }
}

/// Writes `bytes` to `path` unless the file holds exactly them already;
/// true when it wrote. atomic_write adds the fsync the old temp+rename
/// lacked: without it the rename can outlive a crash that the data
/// doesn't.
pub fn write_if_changed(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<bool> {
    if fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    crate::fsutil::atomic_write(path, bytes).map(|()| true)
}

/// What a write came to, for one who asked to hear: Ok, or the line
/// "could not write <path>: <why>".
pub type Written = Result<(), String>;
type Done = Box<dyn FnOnce(Written) + Send>;

struct Pending {
    path: PathBuf,
    bytes: Vec<u8>,
    done: Vec<Done>,
}

#[derive(Default)]
struct SaverState {
    pending: Option<Pending>,
    busy: bool,
    stop: bool,
}

/// The daemon's config writes, off its main loop: a write's fsync can
/// take seconds on a busy disk, and the loop also answers push to talk.
/// Only the latest snapshot is kept (a burst of changes is one write),
/// and an unchanged file is not written at all. Dropping it (quit,
/// SIGTERM) waits for the last write, so a change just before is kept.
pub struct Saver {
    shared: std::sync::Arc<(std::sync::Mutex<SaverState>, std::sync::Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Saver {
    pub fn spawn() -> Saver {
        Saver::with_writer(|p, b| write_if_changed(p, b).map(|_| ()))
    }

    fn with_writer(
        write: impl Fn(&std::path::Path, &[u8]) -> std::io::Result<()> + Send + 'static,
    ) -> Saver {
        let shared = std::sync::Arc::new((
            std::sync::Mutex::new(SaverState::default()),
            std::sync::Condvar::new(),
        ));
        let theirs = shared.clone();
        let worker = std::thread::Builder::new()
            .name("hushmic-config".into())
            .spawn(move || {
                let (state, cv) = &*theirs;
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(p) = s.pending.take() {
                        s.busy = true;
                        drop(s);
                        let r = write(&p.path, &p.bytes)
                            .map_err(|e| format!("could not write {}: {e}", p.path.display()));
                        if p.done.is_empty() {
                            if let Err(e) = &r {
                                eprintln!("[hushmic] {e}");
                            }
                        }
                        for d in p.done {
                            d(r.clone());
                        }
                        s = state.lock().unwrap_or_else(|e| e.into_inner());
                        s.busy = false;
                        cv.notify_all();
                    } else if s.stop {
                        return;
                    } else {
                        s = cv.wait(s).unwrap_or_else(|e| e.into_inner());
                    }
                }
            })
            .expect("spawn the config writer");
        Saver {
            shared,
            worker: Some(worker),
        }
    }

    /// Writes `cfg` soon, on the writer's thread.
    pub fn save(&self, cfg: &Config) {
        self.queue(Config::path(), cfg.bytes(), None);
    }

    /// As [`Saver::save`], then `done` hears how the write that covers
    /// this snapshot went (on the writer's thread).
    pub fn save_then(&self, cfg: &Config, done: impl FnOnce(Written) + Send + 'static) {
        self.queue(Config::path(), cfg.bytes(), Some(Box::new(done)));
    }

    fn queue(&self, path: PathBuf, bytes: Vec<u8>, done: Option<Done>) {
        let (state, cv) = &*self.shared;
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        match s.pending.as_mut() {
            // a newer snapshot replaces the waiting one; whoever asked
            // about that one hears about this
            Some(p) => {
                p.path = path;
                p.bytes = bytes;
                p.done.extend(done);
            }
            None => {
                s.pending = Some(Pending {
                    path,
                    bytes,
                    done: done.into_iter().collect(),
                })
            }
        }
        cv.notify_all();
    }

    /// Waits until everything asked for is written.
    pub fn flush(&self) {
        let (state, cv) = &*self.shared;
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        while s.pending.is_some() || s.busy {
            s = cv.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        self.flush();
        let (state, cv) = &*self.shared;
        state.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        cv.notify_all();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hushmic-cfgsave-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_unchanged_config_is_not_written_again() {
        use std::os::unix::fs::MetadataExt;
        let d = scratch("skip");
        let p = d.join("config.toml");
        let c = Config::default();
        assert!(
            write_if_changed(&p, &c.bytes()).unwrap(),
            "a new file is written"
        );
        let before = fs::metadata(&p).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!write_if_changed(&p, &c.bytes()).unwrap());
        let after = fs::metadata(&p).unwrap();
        // atomic_write renames a new file in: a write changes the inode
        assert_eq!(
            (before.ino(), before.mtime_nsec(), before.mtime()),
            (after.ino(), after.mtime_nsec(), after.mtime())
        );
        let mut changed = c.clone();
        changed.attn_limit = 42.0;
        assert!(write_if_changed(&p, &changed.bytes()).unwrap());
        assert_ne!(fs::metadata(&p).unwrap().ino(), before.ino());
        let _ = fs::remove_dir_all(&d);
    }

    /// A writer that takes `slow` per write and counts the writes.
    fn slow_saver(
        slow: std::time::Duration,
    ) -> (Saver, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let writes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = writes.clone();
        let saver = Saver::with_writer(move |p, b| {
            std::thread::sleep(slow);
            n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            write_if_changed(p, b).map(|_| ())
        });
        (saver, writes)
    }

    #[test]
    fn the_saver_never_holds_up_its_caller_and_keeps_the_latest() {
        use std::time::{Duration, Instant};
        let d = scratch("latest");
        let p = d.join("config.toml");
        let (saver, writes) = slow_saver(Duration::from_millis(300));
        let mut c = Config::default();
        let t = Instant::now();
        for i in 0..20 {
            c.attn_limit = i as f32;
            saver.queue(p.clone(), c.bytes(), None);
        }
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "{:?}",
            t.elapsed()
        );
        saver.flush();
        // the first write, then one for everything that came during it
        assert!(writes.load(std::sync::atomic::Ordering::SeqCst) <= 2);
        let back: Config = toml::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back.attn_limit, 19.0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn snapshots_that_wait_together_are_one_write_and_all_heard() {
        use std::time::Duration;
        let d = scratch("merge");
        let p = d.join("config.toml");
        let (saver, writes) = slow_saver(Duration::from_millis(300));
        let mut c = Config::default();
        // the first is being written while the next three wait: one write
        // covers them, and each one's asker hears it
        saver.queue(p.clone(), c.bytes(), None);
        std::thread::sleep(Duration::from_millis(100));
        let (tx, rx) = std::sync::mpsc::channel();
        for i in 1..=3 {
            c.attn_limit = i as f32;
            let tx = tx.clone();
            saver.queue(
                p.clone(),
                c.bytes(),
                Some(Box::new(move |r| tx.send(r).unwrap())),
            );
        }
        drop(tx);
        saver.flush();
        assert_eq!(rx.iter().collect::<Vec<_>>(), [Ok(()), Ok(()), Ok(())]);
        assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 2);
        let back: Config = toml::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back.attn_limit, 3.0);
        drop(saver);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_change_right_before_quit_is_written_and_heard() {
        use std::time::Duration;
        let d = scratch("flush");
        let p = d.join("config.toml");
        let (saver, _) = slow_saver(Duration::from_millis(200));
        let mut c = Config {
            attn_limit: 7.0,
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let t2 = tx.clone();
        saver.queue(
            p.clone(),
            c.bytes(),
            Some(Box::new(move |r| t2.send(r).unwrap())),
        );
        c.attn_limit = 8.0;
        saver.queue(
            p.clone(),
            c.bytes(),
            Some(Box::new(move |r| tx.send(r).unwrap())),
        );
        // quit: dropping it waits for the write
        drop(saver);
        let back: Config = toml::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back.attn_limit, 8.0);
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [Ok(()), Ok(())]);
        // a write that fails says where and why, to whoever asked
        let (saver, _) = slow_saver(Duration::ZERO);
        let (tx, rx) = std::sync::mpsc::channel();
        fs::write(d.join("a-file"), b"").unwrap();
        let nowhere = d.join("a-file").join("config.toml");
        saver.queue(
            nowhere,
            c.bytes(),
            Some(Box::new(move |r| tx.send(r).unwrap())),
        );
        drop(saver);
        let e = rx.recv().unwrap().unwrap_err();
        assert!(
            e.starts_with("could not write ") && e.contains("a-file"),
            "{e}"
        );
        let _ = fs::remove_dir_all(&d);
    }

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
    fn close_notice_sent_stays_absent_until_sent() {
        let plain = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!plain.contains("close_notice_sent"), "{plain}");
        let c = Config {
            close_notice_sent: true,
            ..Config::default()
        };
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert!(back.close_notice_sent);
        let old: Config = toml::from_str("enabled = true\n").unwrap();
        assert!(!old.close_notice_sent);
    }

    #[test]
    fn shortcuts_setup_defaults_false_and_stays_absent_until_used() {
        // Existing configs stay byte-identical until the user sets up
        // shortcuts once; afterwards the bool round-trips.
        assert!(!Config::default().shortcuts_setup);
        let plain = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!plain.contains("shortcuts_setup"), "{plain}");
        let c = Config {
            shortcuts_setup: true,
            ..Config::default()
        };
        let s = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert!(back.shortcuts_setup);
        let old = "enabled = true\n";
        let c: Config = toml::from_str(old).unwrap();
        assert!(!c.shortcuts_setup);
    }

    #[test]
    fn tray_and_notifications_default_true_and_stay_absent_until_changed() {
        let d = Config::default();
        assert!(d.tray && d.notifications);
        let plain = toml::to_string_pretty(&d).unwrap();
        assert!(!plain.contains("tray"), "{plain}");
        assert!(!plain.contains("notifications"), "{plain}");
        let c = Config {
            tray: false,
            notifications: false,
            ..Config::default()
        };
        let s = toml::to_string_pretty(&c).unwrap();
        assert!(s.contains("tray = false"), "{s}");
        assert!(s.contains("notifications = false"), "{s}");
        let back: Config = toml::from_str(&s).unwrap();
        assert!(!back.tray && !back.notifications);
        let old: Config = toml::from_str("enabled = true\n").unwrap();
        assert!(old.tray && old.notifications);
    }

    #[test]
    fn tray_icon_defaults_to_auto_and_stays_absent_until_changed() {
        let d = Config::default();
        assert_eq!(d.tray_icon, TrayIcon::Auto);
        let plain = toml::to_string_pretty(&d).unwrap();
        assert!(!plain.contains("tray_icon"), "{plain}");
        for (v, word) in [(TrayIcon::Color, "color"), (TrayIcon::Symbolic, "symbolic")] {
            let c = Config {
                tray_icon: v,
                ..Config::default()
            };
            let s = toml::to_string_pretty(&c).unwrap();
            assert!(s.contains(&format!("tray_icon = \"{word}\"")), "{s}");
            let back: Config = toml::from_str(&s).unwrap();
            assert_eq!(back.tray_icon, v);
        }
        let old: Config = toml::from_str("enabled = true\n").unwrap();
        assert_eq!(old.tray_icon, TrayIcon::Auto);
    }

    #[test]
    fn an_unknown_tray_icon_falls_back_without_losing_the_file() {
        // A hand-edited typo (or a value of the wrong type) costs that one
        // field, not the whole document — load() would move the file aside.
        for bad in [
            "tray_icon = \"mono\"\n",
            "tray_icon = \"SYMBOLIC\"\n",
            "tray_icon = true\n",
            "tray_icon = 3\n",
        ] {
            let c: Config =
                toml::from_str(&format!("attn_limit = 24.0\n{bad}")).unwrap_or_else(|e| {
                    panic!("{bad} should not fail the document: {e}");
                });
            assert_eq!(c.tray_icon, TrayIcon::Auto, "{bad}");
            assert_eq!(c.attn_limit, 24.0, "{bad}");
        }
    }

    #[test]
    fn voice_hold_defaults_to_off_and_stays_absent_until_on() {
        let d = Config::default();
        assert!(!d.voice_hold);
        assert!(!toml::to_string_pretty(&d).unwrap().contains("voice_hold"));
        let on = Config {
            voice_hold: true,
            ..Config::default()
        };
        let s = toml::to_string_pretty(&on).unwrap();
        assert!(s.contains("voice_hold = true"), "{s}");
        assert!(toml::from_str::<Config>(&s).unwrap().voice_hold);
        // a file from before the key
        assert!(
            !toml::from_str::<Config>("enabled = true\n")
                .unwrap()
                .voice_hold
        );
    }

    #[test]
    fn inference_defaults_to_auto_stays_absent_and_is_lenient() {
        let d = Config::default();
        assert_eq!(d.inference, Inference::Auto);
        assert!(!toml::to_string_pretty(&d).unwrap().contains("inference"));
        let c = Config {
            inference: Inference::Onnx,
            ..Config::default()
        };
        let s = toml::to_string_pretty(&c).unwrap();
        assert!(s.contains("inference = \"onnx\""), "{s}");
        assert_eq!(
            toml::from_str::<Config>(&s).unwrap().inference,
            Inference::Onnx
        );
        for bad in ["inference = \"native\"\n", "inference = false\n"] {
            let c: Config = toml::from_str(&format!("attn_limit = 24.0\n{bad}")).unwrap();
            assert_eq!(c.inference, Inference::Auto, "{bad}");
            assert_eq!(c.attn_limit, 24.0, "{bad}");
        }
        for w in Inference::VALUES {
            assert_eq!(Inference::parse(w).map(Inference::as_str), Some(w));
        }
    }

    #[test]
    fn appearance_and_hidden_notices_stay_absent_until_used() {
        let d = Config::default();
        assert_eq!(d.appearance, Appearance::System);
        let plain = toml::to_string_pretty(&d).unwrap();
        assert!(!plain.contains("appearance"), "{plain}");
        assert!(!plain.contains("hidden_notices"), "{plain}");
        let c = Config {
            appearance: Appearance::Light,
            hidden_notices: vec![
                HiddenNotice {
                    app: "com.obsproject.Studio".into(),
                    name: Some("OBS Studio".into()),
                },
                HiddenNotice {
                    app: "chromium".into(),
                    name: None,
                },
            ],
            ..Config::default()
        };
        let s = toml::to_string_pretty(&c).unwrap();
        assert!(s.contains("appearance = \"light\""), "{s}");
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.appearance, Appearance::Light);
        assert_eq!(back.hidden_notices, c.hidden_notices);
        for w in Appearance::VALUES {
            assert_eq!(Appearance::parse(w).map(Appearance::as_str), Some(w));
        }
    }

    #[test]
    fn a_bad_appearance_or_notice_entry_costs_only_itself() {
        let c: Config = toml::from_str(
            "attn_limit = 24.0\nappearance = \"sepia\"\n\
             [[hidden_notices]]\nname = 3\n\
             [[hidden_notices]]\napp = \"obs\"\nname = 4\n",
        )
        .unwrap_or_else(|e| panic!("must not fail the document: {e}"));
        assert_eq!(c.appearance, Appearance::System);
        assert_eq!(c.attn_limit, 24.0);
        assert_eq!(
            c.hidden_notices,
            vec![HiddenNotice {
                app: "obs".into(),
                name: None
            }]
        );
        let c: Config = toml::from_str("hidden_notices = 7\n").unwrap();
        assert!(c.hidden_notices.is_empty());
    }

    #[test]
    fn tray_icon_words_round_trip() {
        for w in TrayIcon::VALUES {
            let v = TrayIcon::parse(w).unwrap_or_else(|| panic!("{w} must parse"));
            assert_eq!(v.as_str(), w);
        }
        assert_eq!(TrayIcon::parse("mono"), None);
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
    fn selection_changes_only_the_mic() {
        let mut c = with_entry("rode", "dpdfnet2_48khz_hr", 24.0);
        c.apply_mic_selection(Some("rode".into()));
        assert_eq!(c.mic.as_deref(), Some("rode"));
        // The profile applies through the resolver, not by being copied
        // into the defaults every other device runs with.
        assert_eq!(c.model, "dpdfnet8_48khz_hr");
        assert_eq!(c.attn_limit, 100.0);
        assert_eq!(c.active_profile(Some("rode"), None).attn_limit, 24.0);
        c.apply_mic_selection(Some("webcam".into()));
        assert!(!c.mic_prefs.contains_key("webcam"));
        c.apply_mic_selection(None);
        assert_eq!(c.mic, None);
    }

    /// The reported setup: nothing pinned, defaults dpdfnet2/24, a RODE
    /// profile dpdfnet8/100 and a Jabra profile dpdfnet2/24.
    fn reported_setup() -> Config {
        let mut c = with_entry("rode", "dpdfnet8_48khz_hr", 100.0);
        c.mic_prefs.insert(
            "jabra".into(),
            MicPrefs {
                model: "dpdfnet2_48khz_hr".into(),
                attn_limit: 24.0,
            },
        );
        c.model = "dpdfnet2_48khz_hr".into();
        c.attn_limit = 24.0;
        c
    }

    #[test]
    fn active_profile_in_every_mode() {
        let c = reported_setup();
        let p = |m: &str, a: f32| (m.to_string(), a);
        let got = |ap: ActiveProfile| (ap.model, ap.attn_limit);
        // Follow-default: the default source's profile.
        let a = c.active_profile(None, Some("rode"));
        assert_eq!((a.device.as_deref(), a.saved), (Some("rode"), true));
        assert_eq!(got(a), p("dpdfnet8_48khz_hr", 100.0));
        assert_eq!(
            got(c.active_profile(None, Some("jabra"))),
            p("dpdfnet2_48khz_hr", 24.0)
        );
        // Follow-default on a device without a profile: the defaults, but
        // the device is still the one in use (edits will land there).
        let a = c.active_profile(None, Some("webcam"));
        assert_eq!((a.device.as_deref(), a.saved), (Some("webcam"), false));
        assert_eq!(got(a), p("dpdfnet2_48khz_hr", 24.0));
        // Pinned: the pinned mic's profile — never the default source's.
        assert_eq!(
            got(c.active_profile(Some("rode"), Some("jabra"))),
            p("dpdfnet8_48khz_hr", 100.0)
        );
        let a = c.active_profile(Some("webcam"), Some("rode"));
        assert_eq!((a.device.as_deref(), a.saved), (Some("webcam"), false));
        assert_eq!(got(a), p("dpdfnet2_48khz_hr", 24.0));
        // Unknown default, our own node, a monitor: nobody owns the
        // settings, the defaults apply.
        for d in [None, Some("hushmic_source"), Some("alsa_output.x.monitor")] {
            let a = c.active_profile(None, d);
            assert_eq!(a.device, None, "{d:?}");
            assert!(!a.saved);
            assert_eq!(got(a), p("dpdfnet2_48khz_hr", 24.0), "{d:?}");
        }
    }

    #[test]
    fn edits_land_in_the_settings_in_effect() {
        let mut c = reported_setup();
        // The reported bug: RODE is the default, the tray edit must reach
        // the RODE profile the chain runs — not the defaults.
        c.set_attn_for(Some("rode"), 12.0);
        assert_eq!(c.mic_prefs["rode"].attn_limit, 12.0);
        assert_eq!(c.attn_limit, 24.0);
        assert_eq!(c.active_profile(None, Some("rode")).attn_limit, 12.0);
        c.set_model_for(Some("rode"), "dpdfnet2_48khz_hr".into());
        assert_eq!(c.mic_prefs["rode"].model, "dpdfnet2_48khz_hr");
        assert_eq!(c.mic_prefs["rode"].attn_limit, 12.0);
        // Following the default onto a device without a profile: the
        // edit creates that device's own (UX panel B2), the defaults stay
        c.set_attn_for(Some("webcam"), 6.0);
        assert_eq!(c.mic_prefs["webcam"].attn_limit, 6.0);
        assert_eq!(
            c.mic_prefs["webcam"].model, c.model,
            "seeded from the defaults"
        );
        assert_eq!(c.attn_limit, 24.0);
        assert_eq!(c.mic_prefs["jabra"].attn_limit, 24.0);
    }

    #[test]
    fn an_edit_on_the_pinned_mic_creates_its_profile() {
        let mut c = reported_setup();
        c.mic = Some("webcam".into());
        assert_eq!(c.edit_target(Some("webcam")), Some("webcam"));
        c.set_attn_for(Some("webcam"), 6.0);
        // Seeded from the defaults it ran with: only the edit differs.
        assert_eq!(
            c.mic_prefs["webcam"],
            MicPrefs {
                model: "dpdfnet2_48khz_hr".into(),
                attn_limit: 6.0
            }
        );
        assert_eq!(c.attn_limit, 24.0);
        // Pinned mic unplugged, chain on the default: the device in use,
        // as everywhere
        assert_eq!(c.edit_target(Some("other")), Some("other"));
        assert_eq!(c.edit_target(Some("jabra")), Some("jabra"));
    }

    #[test]
    fn edits_without_an_owner_change_the_defaults() {
        let mut c = reported_setup();
        let before = c.mic_prefs.clone();
        for d in [None, Some("hushmic_source"), Some("x.monitor"), Some("")] {
            // Not even when pinned: our own node never gets a profile.
            c.mic = d.map(str::to_string);
            c.set_attn_for(d, 6.0);
            c.set_model_for(d, "dpdfnet8_48khz_hr".into());
        }
        assert_eq!(c.mic_prefs, before, "no profile for our node or None");
        assert_eq!(c.attn_limit, 6.0);
        assert_eq!(c.model, "dpdfnet8_48khz_hr");
    }

    #[test]
    fn describe_names_whose_settings_apply() {
        let c = reported_setup();
        assert_eq!(
            c.active_profile(None, Some("rode"))
                .describe(Some("RODE NT-USB")),
            "RODE NT-USB profile (dpdfnet8, 100 dB)"
        );
        assert_eq!(
            c.active_profile(None, Some("jabra")).describe(None),
            "jabra profile (dpdfnet2, 24 dB)"
        );
        assert_eq!(
            c.active_profile(None, Some("webcam"))
                .describe(Some("Webcam")),
            "defaults for Webcam (dpdfnet2, 24 dB)"
        );
        assert_eq!(
            c.active_profile(None, None).describe(None),
            "defaults (dpdfnet2, 24 dB)"
        );
    }
}
