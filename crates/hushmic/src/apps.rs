//! Which apps record what: the apps using HushMic (for the Live page's "in
//! a call" and first run's "Firefox is using HushMic."), and the apps that
//! record the microphone HushMic cleans directly (the "Direct mic" notice).
//! Both come from the `pw-dump` snapshot the daemon already takes every
//! tick ([`crate::pipewire::parse_readers`]); this module only judges.

use crate::appstate::{AppRef, DirectUse};
use crate::pipewire::{Reader, StreamNode};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Names that say nothing about the app (Electron and WebRTC engines,
/// ALSA shims): shown as "An app".
const GENERIC_NAMES: [&str; 6] = [
    "webrtc voiceengine",
    "chromium input",
    "pipewire alsa",
    "alsa plug-in",
    "pulseaudio",
    "pw-cat",
];

/// Settings and mixers that open plain streams to draw level meters (a
/// second net behind `stream.monitor`).
const METER_APPS: [&str; 6] = [
    "org.pulseaudio.pavucontrol",
    "pavucontrol",
    "org.kde.plasma-pa",
    "plasmashell",
    "org.gnome.settings",
    "gnome-control-center",
];

/// EasyEffects' own capture (it processes the mic itself; HushMic's
/// notice is not for it).
const EASYEFFECTS: [&str; 2] = ["com.github.wwmm.easyeffects", "easyeffects"];

/// Programs that run someone else's app: their name is not the app's.
/// Matched whole, a version suffix aside (`python3.12`, `electron28`):
/// `shotcut` or `monodevelop` are apps.
const RUNTIMES: [&str; 13] = [
    "electron",
    "python",
    "java",
    "node",
    "nodejs",
    "wine",
    "wine-preloader",
    "wine64-preloader",
    "mono",
    "bash",
    "sh",
    "flatpak",
    "bwrap",
];

/// A runtime's program name, whole or with a version suffix.
fn is_runtime(program: &str) -> bool {
    let p = program.to_ascii_lowercase();
    let bare = p.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    RUNTIMES.contains(&p.as_str()) || RUNTIMES.contains(&bare)
}

fn lower(s: &Option<String>) -> String {
    s.as_deref().unwrap_or_default().to_ascii_lowercase()
}

/// Display names from the desktop entries (`*.desktop` Name=), for apps
/// whose stream reports no usable name. Indexed by file name (a Flatpak's
/// app id), by program (Exec=) and by window class. The scan never runs on
/// the caller's thread (the daemon's main loop also carries push-to-talk):
/// it runs in the background at the start and again, at most once a
/// minute, after a lookup missed; a lookup answers from the last index.
pub struct Names {
    dirs: Arc<Vec<PathBuf>>,
    index: Arc<Mutex<Option<Arc<DesktopIndex>>>>,
    scanning: Arc<AtomicBool>,
    /// When the last scan started.
    scanned_at: Mutex<Option<Instant>>,
}

#[derive(Default)]
struct DesktopIndex {
    by_file: HashMap<String, String>,
    by_program: HashMap<String, String>,
}

const RESCAN_AFTER: Duration = Duration::from_secs(60);

impl Names {
    /// The session's application dirs: XDG data home and dirs, plus the
    /// Flatpak exports (not always on XDG_DATA_DIRS).
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        let data_dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let mut roots: Vec<PathBuf> = data_home.into_iter().collect();
        roots.extend(data_dirs.split(':').map(PathBuf::from));
        roots.extend(home.map(|h| h.join(".local/share/flatpak/exports/share")));
        roots.push("/var/lib/flatpak/exports/share".into());
        roots.push("/var/lib/snapd/desktop".into());
        let mut dirs: Vec<PathBuf> = Vec::new();
        for d in roots.into_iter().map(|r| r.join("applications")) {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        let names = Names::with_dirs(dirs);
        names.rescan_if_due();
        names
    }

    /// Names from these dirs; nothing is scanned until a lookup misses.
    pub fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        Names {
            dirs: Arc::new(dirs),
            index: Arc::new(Mutex::new(None)),
            scanning: Arc::new(AtomicBool::new(false)),
            scanned_at: Mutex::new(None),
        }
    }

    /// Scan now, on this thread (tests).
    pub fn scan_now(&self) {
        let ix = scan(&self.dirs);
        *lock(&self.index) = Some(Arc::new(ix));
        *lock(&self.scanned_at) = Some(Instant::now());
    }

    /// The Name of the desktop entry for a Flatpak app id or a program, as
    /// the last scan saw it. Never scans on this thread.
    pub fn desktop_name(&self, key: &str) -> Option<String> {
        let key = key.to_ascii_lowercase();
        let ix = lock(&self.index).clone();
        let found = ix.and_then(|ix| {
            ix.by_file
                .get(&key)
                .or_else(|| ix.by_program.get(&key))
                .cloned()
        });
        if found.is_none() {
            self.rescan_if_due();
        }
        found
    }

    /// Start a background scan unless one runs or one started under a
    /// minute ago.
    fn rescan_if_due(&self) {
        {
            let mut at = lock(&self.scanned_at);
            if at.is_some_and(|t| t.elapsed() < RESCAN_AFTER) {
                return;
            }
            if self.scanning.swap(true, Ordering::AcqRel) {
                return;
            }
            *at = Some(Instant::now());
        }
        let (dirs, index, scanning) =
            (self.dirs.clone(), self.index.clone(), self.scanning.clone());
        std::thread::spawn(move || {
            let ix = scan(&dirs);
            *lock(&index) = Some(Arc::new(ix));
            scanning.store(false, Ordering::Release);
        });
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn scan(dirs: &[PathBuf]) -> DesktopIndex {
    let mut ix = DesktopIndex::default();
    for dir in dirs {
        scan_dir(dir, &mut ix, 1);
    }
    ix
}

/// One dir of entries (and its subdirs, one level: `applications/kde4/`).
/// The first dir that names a key wins, as XDG lookup order says.
fn scan_dir(dir: &Path, ix: &mut DesktopIndex, depth: u8) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            if depth > 0 {
                scan_dir(&path, ix, depth - 1);
            }
            continue;
        }
        if path.extension().and_then(|x| x.to_str()) != Some("desktop") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(entry) = parse_desktop(&text) else {
            continue;
        };
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            ix.by_file
                .entry(stem.to_ascii_lowercase())
                .or_insert_with(|| entry.name.clone());
        }
        for p in entry.programs {
            ix.by_program
                .entry(p.to_ascii_lowercase())
                .or_insert_with(|| entry.name.clone());
        }
    }
}

struct DesktopEntry {
    name: String,
    /// The program Exec= runs (its file name) and StartupWMClass.
    programs: Vec<String>,
}

/// The `[Desktop Entry]` group's Name, program and window class; None for
/// a hidden entry or one without a name.
fn parse_desktop(text: &str) -> Option<DesktopEntry> {
    let mut in_entry = false;
    let (mut name, mut exec, mut class, mut hidden) = (None, None, None, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "Name" => name = Some(v.trim().to_string()),
            "Exec" => exec = Some(v.trim().to_string()),
            "StartupWMClass" => class = Some(v.trim().to_string()),
            "Hidden" => hidden = v.trim() == "true",
            _ => {}
        }
    }
    let name = name.filter(|n| !n.is_empty() && !hidden)?;
    let mut programs: Vec<String> = Vec::new();
    if let Some(program) = exec.as_deref().and_then(exec_program) {
        programs.push(program);
    }
    programs.extend(class);
    Some(DesktopEntry { name, programs })
}

/// The program an Exec= line runs, past `env` and its assignments.
fn exec_program(exec: &str) -> Option<String> {
    let words = exec_words(exec);
    let mut words = words.iter().map(String::as_str);
    let mut first = words.next()?;
    if Path::new(first).file_name().and_then(|f| f.to_str()) == Some("env") {
        loop {
            first = words.next()?;
            match first {
                // the one option with an argument: -u NAME
                "-u" | "--unset" => {
                    words.next()?;
                }
                w if w.contains('=') || w.starts_with('-') => {}
                _ => break,
            }
        }
    }
    let program = Path::new(first).file_name()?.to_str()?;
    (!program.is_empty()).then(|| program.to_string())
}

/// Exec= split into words: double quotes keep spaces, a backslash in them
/// escapes the next character (the desktop entry spec's quoting).
fn exec_words(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut started) = (false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => word.extend(chars.next()),
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// An app's key and display name. The key prefers what survives an update
/// (Flatpak app id, then binary, then the reported name). The name is the
/// reported one unless it is generic, else the desktop entry's (by Flatpak
/// app id, then by program), else the program's own name; None ("An app")
/// when nothing says more than a runtime would.
pub fn app_ref(r: &Reader, names: &Names) -> AppRef {
    let generic = |n: &str| {
        let n = n.to_ascii_lowercase();
        GENERIC_NAMES.iter().any(|g| n.starts_with(g))
    };
    // The program, unless it is a runtime running someone else's code (or
    // one of the generic ones, by its whole name).
    let program = r
        .binary
        .as_deref()
        .filter(|b| !is_runtime(b) && !GENERIC_NAMES.contains(&b.to_ascii_lowercase().as_str()));
    let name = r
        .app_name
        .clone()
        .filter(|n| !generic(n))
        .or_else(|| {
            r.flatpak_id
                .as_deref()
                .and_then(|id| names.desktop_name(id))
        })
        .or_else(|| program.and_then(|p| names.desktop_name(p)))
        .or_else(|| program.map(str::to_string))
        .map(|n| clean(&n, MAX_NAME))
        .filter(|n| !n.is_empty());
    let id = r
        .flatpak_id
        .clone()
        .or_else(|| r.binary.clone())
        .or_else(|| r.app_name.clone())
        .unwrap_or_else(|| r.node_name.clone());
    AppRef {
        id: clean(&id, MAX_ID)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("_"),
        name,
    }
}

/// Longest app name shown (and saved with a hidden notice).
const MAX_NAME: usize = 64;
/// Longest app key.
const MAX_ID: usize = 128;

/// What an app says about itself is untrusted text: no control characters
/// (newlines would reach the config and the window), and bounded.
fn clean(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// A capture stream of an app (not a meter, a processing stage, EasyEffects
/// or one of ours), moving audio right now.
fn is_app_capture(r: &Reader, ours: &dyn Fn(&Reader) -> bool) -> bool {
    r.state == "running" && is_app_stream(r, ours)
}

/// A capture stream of an app, running or not.
fn is_app_stream(r: &Reader, ours: &dyn Fn(&Reader) -> bool) -> bool {
    let ids = [
        lower(&r.flatpak_id),
        lower(&r.app_id),
        lower(&r.binary),
        lower(&r.app_name),
    ];
    r.media_class == "Stream/Input/Audio"
        && !r.monitor
        && !r.stage
        && !r.node_name.starts_with("hushmic")
        && !r.node_name.starts_with("easyeffects_")
        && !r.node_name.starts_with("ee_")
        && !ids
            .iter()
            .any(|i| METER_APPS.contains(&i.as_str()) || EASYEFFECTS.contains(&i.as_str()))
        && !ours(r)
}

/// Something that is not an app records HushMic and keeps its engine
/// running: Mic check, a volume control's level meter, a monitor, a
/// processing stage (what [`using`] leaves out, while it runs).
pub fn others_running(readers: &[Reader], ours: &dyn Fn(&Reader) -> bool) -> bool {
    readers
        .iter()
        .any(|r| r.state == "running" && !is_app_capture(r, ours))
}

/// The apps recording from HushMic now, one entry per app.
pub fn using(readers: &[Reader], ours: &dyn Fn(&Reader) -> bool, names: &Names) -> Vec<AppRef> {
    let mut out: Vec<AppRef> = Vec::new();
    for r in readers.iter().filter(|r| is_app_capture(r, ours)) {
        let a = app_ref(r, names);
        if !out.iter().any(|o| o.id == a.id) {
            out.push(a);
        }
    }
    out
}

/// The apps playing through a sink now (`writers`: its
/// [`crate::pipewire::parse_writers`]), one entry per app: running output
/// streams, not HushMic's own, not a processing stage.
pub fn playing(writers: &[Reader], ours: &dyn Fn(&Reader) -> bool, names: &Names) -> Vec<AppRef> {
    let mut out: Vec<AppRef> = Vec::new();
    for r in writers.iter().filter(|r| {
        r.state == "running"
            && r.media_class == "Stream/Output/Audio"
            && !r.monitor
            && !r.stage
            && !r.node_name.starts_with("hushmic")
            && !ours(r)
    }) {
        let a = app_ref(r, names);
        if !out.iter().any(|o| o.id == a.id) {
            out.push(a);
        }
    }
    out
}

/// The apps' capture streams among `readers`, running or corked: what Off
/// moves off HushMic (UX panel A1).
pub fn app_streams(readers: &[Reader], ours: &dyn Fn(&Reader) -> bool) -> Vec<StreamNode> {
    readers
        .iter()
        .filter(|r| is_app_stream(r, ours))
        .map(StreamNode::of)
        .collect()
}

/// Which streams to move back onto HushMic now it is on again (UX panel
/// A1): of those Off moved off it (`moved`), the ones still in the graph
/// (`streams`: the same node of the same app) that record something else
/// than HushMic (`on_hushmic`). Never another stream: an app that chose
/// the raw microphone itself stays on it. Pure.
pub fn to_return(
    moved: &[StreamNode],
    streams: &[StreamNode],
    on_hushmic: &[StreamNode],
) -> Vec<u32> {
    moved
        .iter()
        .filter(|m| streams.iter().any(|s| m.same(s)) && !on_hushmic.iter().any(|h| h.id == m.id))
        .map(|m| m.id)
        .collect()
}

/// How long an app's stream must have been running before it is named:
/// with a 5 s tick that is the second tick that sees it (5 to 10 s), and
/// early ticks (after a command) cannot shorten it.
pub const DIRECT_AFTER: Duration = Duration::from_secs(4);

/// The direct-mic debounce: an app counts once its stream has been seen
/// running on two looks in a row, at least [`DIRECT_AFTER`] apart, so a
/// short probe or a level check never raises the notice; it goes as soon
/// as the stream stops.
#[derive(Default)]
pub struct DirectWatch {
    /// App id → when it was first seen in this streak.
    since: HashMap<String, Instant>,
}

impl DirectWatch {
    /// Feed one tick's readers of the microphone HushMic cleans; returns
    /// the apps to name, hidden ones left out. `enabled` false (HushMic off
    /// or not cleaning a known mic) clears everything.
    pub fn observe(
        &mut self,
        readers: &[Reader],
        enabled: bool,
        hidden: &dyn Fn(&str) -> bool,
        ours: &dyn Fn(&Reader) -> bool,
        names: &Names,
    ) -> Vec<DirectUse> {
        self.observe_at(readers, enabled, hidden, ours, names, Instant::now())
    }

    /// [`DirectWatch::observe`] at a given moment.
    pub fn observe_at(
        &mut self,
        readers: &[Reader],
        enabled: bool,
        hidden: &dyn Fn(&str) -> bool,
        ours: &dyn Fn(&Reader) -> bool,
        names: &Names,
        now: Instant,
    ) -> Vec<DirectUse> {
        if !enabled {
            self.since.clear();
            return Vec::new();
        }
        let mut seen: Vec<DirectUse> = Vec::new();
        for r in readers.iter().filter(|r| is_app_capture(r, ours)) {
            let app = app_ref(r, names);
            if seen.iter().any(|d| d.app.id == app.id) {
                continue;
            }
            seen.push(DirectUse {
                app,
                follows_default: r.target.is_none(),
            });
        }
        self.since
            .retain(|id, _| seen.iter().any(|d| &d.app.id == id));
        for d in &seen {
            self.since.entry(d.app.id.clone()).or_insert(now);
        }
        seen.into_iter()
            .filter(|d| {
                self.since
                    .get(&d.app.id)
                    .is_some_and(|t| now.saturating_duration_since(*t) >= DIRECT_AFTER)
            })
            .filter(|d| !hidden(&d.app.id))
            .collect()
    }
}

/// `pid` is this process or one of its descendants (the window, and the
/// recorders it starts): our own streams are never an app's.
pub fn is_own_pid(pid: u32) -> bool {
    let me = std::process::id();
    let mut p = pid;
    for _ in 0..4 {
        if p == me {
            return true;
        }
        match parent_pid(p) {
            Some(pp) if pp > 1 => p = pp,
            _ => return false,
        }
    }
    false
}

fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may contain anything: parse
    // after the last ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// The programs our own streams run in: the recorders and players we
/// start, and this binary (the window's in-process streams).
const OWN_PROGRAMS: [&str; 3] = ["pw-record", "pw-cat", "pw-play"];

fn own_program_name() -> Option<String> {
    std::env::current_exe()
        .ok()?
        .file_name()?
        .to_str()
        .map(str::to_string)
}

/// The production "ours" test: (inside a Flatpak) a stream of our own
/// sandbox, or one of our recorders / this binary running under the daemon.
/// Ancestry alone is not enough: an app HushMic happened to start (a
/// browser opened from a link) is the user's, not ours.
pub fn is_ours(r: &Reader) -> bool {
    if let (Some(id), Some(me)) = (r.flatpak_id.as_deref(), crate::sandbox::flatpak_app_id()) {
        if id == me {
            return true;
        }
    }
    let own_program = r.binary.as_deref().is_some_and(|b| {
        OWN_PROGRAMS.contains(&b) || own_program_name().as_deref() == Some(b) || b == "hushmic"
    });
    own_program && r.pid.is_some_and(is_own_pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(name: &str, app: Option<&str>, binary: Option<&str>) -> Reader {
        Reader {
            node_name: name.into(),
            media_class: "Stream/Input/Audio".into(),
            state: "running".into(),
            app_name: app.map(str::to_string),
            binary: binary.map(str::to_string),
            ..Reader::default()
        }
    }

    fn none(_: &Reader) -> bool {
        false
    }

    /// No desktop entries at all.
    fn no_names() -> Names {
        Names::with_dirs(vec![])
    }

    fn node(id: u32, name: &str, pid: u32) -> StreamNode {
        StreamNode {
            id,
            node_name: name.into(),
            pid: Some(pid),
            serial: None,
        }
    }

    /// UX panel A1: Off remembers the apps' streams on HushMic, corked
    /// ones too, never meters or stages.
    #[test]
    fn off_remembers_the_apps_streams_on_hushmic() {
        let zoom = Reader {
            id: 41,
            pid: Some(7),
            ..stream("zoom_capture", Some("Zoom"), Some("zoom"))
        };
        let corked = Reader {
            id: 42,
            state: "idle".into(),
            ..stream("teams_capture", Some("Teams"), None)
        };
        let meter = Reader {
            id: 43,
            monitor: true,
            ..stream("peak", Some("pavucontrol"), None)
        };
        let stage = Reader {
            id: 44,
            stage: true,
            ..stream("ee_sie", Some("EasyEffects"), None)
        };
        let ids: Vec<u32> = app_streams(&[zoom, corked, meter, stage], &none)
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec![41, 42]);
    }

    /// UX panel A1: On moves back exactly the streams Off moved that are
    /// still there and not back yet; never another stream, never a
    /// reused node id.
    #[test]
    fn on_moves_back_only_what_off_moved() {
        let zoom = node(41, "zoom_capture", 7);
        let teams = node(42, "teams_capture", 8);
        let slack = node(43, "slack_capture", 9);
        let moved = [zoom.clone(), teams.clone()];
        // both still there, on the raw mic; Slack chose the raw mic itself
        let graph = [zoom.clone(), teams.clone(), slack.clone()];
        assert_eq!(to_return(&moved, &graph, &[]), vec![41, 42]);
        // Teams follows the default and is back on HushMic already
        assert_eq!(
            to_return(&moved, &graph, std::slice::from_ref(&teams)),
            vec![41]
        );
        // Zoom left; its id now belongs to another app's stream
        let reused = node(41, "obs_capture", 12);
        assert_eq!(to_return(&moved, &[reused, teams.clone()], &[]), vec![42]);
        assert!(to_return(&[], &graph, &[]).is_empty());
        // a native stream's process known only by its serial on one side
        // (A107 SF-A1): the serial says it is the same stream
        let native = |pid| StreamNode {
            pid,
            serial: Some(500),
            ..node(45, "pw-record", 0)
        };
        assert_eq!(
            to_return(&[native(Some(30))], &[native(None)], &[]),
            vec![45]
        );
        // and a new stream under a freed id and the same name has another
        let mut other = native(None);
        other.serial = Some(501);
        assert!(to_return(&[native(None)], &[other], &[]).is_empty());
    }

    #[test]
    fn apps_are_named_by_what_they_report_unless_it_is_generic() {
        let names = no_names();
        let f = stream("Firefox", Some("Firefox"), Some("firefox"));
        assert_eq!(
            app_ref(&f, &names),
            AppRef {
                id: "firefox".into(),
                name: Some("Firefox".into())
            }
        );
        let e = stream("x", Some("WEBRTC VoiceEngine"), Some("electron"));
        assert_eq!(app_ref(&e, &names).name, None, "a runtime is no name");
        assert_eq!(app_ref(&e, &names).id, "electron");
        let mut z = stream("x", Some("Zoom Workplace"), Some("zoom"));
        z.flatpak_id = Some("us.zoom.Zoom".into());
        assert_eq!(app_ref(&z, &names).id, "us.zoom.Zoom");
        let bare = stream("node 1", None, None);
        assert_eq!(
            app_ref(&bare, &names).id,
            "node_1",
            "keys never carry whitespace"
        );
        // Nothing better known: the program's own name.
        let d = stream("x", Some("WEBRTC VoiceEngine"), Some("Discord"));
        assert_eq!(app_ref(&d, &names).name.as_deref(), Some("Discord"));
    }

    #[test]
    fn others_are_the_running_readers_that_are_no_app() {
        let app = stream("Firefox", Some("Firefox"), Some("firefox"));
        let mut meter = stream("PulseAudio Volume Control", Some("pavucontrol"), None);
        meter.monitor = true;
        let check = stream("hushmic-miccheck", Some("HushMic"), Some("pw-record"));
        assert!(!others_running(std::slice::from_ref(&app), &none));
        assert!(others_running(&[app.clone(), meter.clone()], &none));
        assert!(others_running(&[check], &none));
        meter.state = "idle".into();
        assert!(
            !others_running(&[app, meter], &none),
            "a corked one runs nothing"
        );
    }

    /// A scratch applications dir with these entries (file name, text).
    fn apps_dir(tag: &str, entries: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hushmic-apps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("kde4")).unwrap();
        for (file, text) in entries {
            std::fs::write(dir.join(file), text).unwrap();
        }
        dir
    }

    #[test]
    fn generic_names_fall_back_to_the_desktop_entry() {
        let dir = apps_dir(
            "names",
            &[
                (
                    "com.discordapp.Discord.desktop",
                    "[Desktop Entry]\nName=Discord\nExec=/usr/bin/flatpak run com.discordapp.Discord\n",
                ),
                (
                    "signal.desktop",
                    "[Desktop Entry]\nName=Signal\nExec=env BAMF=1 \"/opt/Signal/signal-desktop\" %U\n\
                     [Desktop Action new]\nName=New window\n",
                ),
                (
                    "kde4/teams.desktop",
                    "[Desktop Entry]\nName=Teams\nExec=teams-app\nStartupWMClass=TeamsWin\n",
                ),
                ("gone.desktop", "[Desktop Entry]\nName=Gone\nExec=gone\nHidden=true\n"),
            ],
        );
        let names = Names::with_dirs(vec![dir.clone()]);
        names.scan_now();
        // A Flatpak by its app id (the entry's file name).
        let mut d = stream("x", Some("WEBRTC VoiceEngine"), Some("Discord"));
        d.flatpak_id = Some("com.discordapp.Discord".into());
        assert_eq!(app_ref(&d, &names).name.as_deref(), Some("Discord"));
        // A program by Exec= (past env and quotes), or by window class.
        let s = stream("x", Some("Chromium input"), Some("signal-desktop"));
        assert_eq!(app_ref(&s, &names).name.as_deref(), Some("Signal"));
        let t = stream("x", None, Some("teamswin"));
        assert_eq!(app_ref(&t, &names).name.as_deref(), Some("Teams"));
        // A hidden entry names nothing; a reported name always wins.
        assert_eq!(names.desktop_name("gone"), None);
        let f = stream("x", Some("Firefox"), Some("signal-desktop"));
        assert_eq!(app_ref(&f, &names).name.as_deref(), Some("Firefox"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A lookup never scans on the caller's thread: a miss starts a scan
    /// in the background, and the name is there on a later look.
    #[test]
    fn a_miss_scans_in_the_background() {
        let dir = apps_dir(
            "background",
            &[(
                "zoom.desktop",
                "[Desktop Entry]\nName=Zoom Workplace\nExec=zoom %U\n",
            )],
        );
        let names = Names::with_dirs(vec![dir.clone()]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut first = None;
        loop {
            let got = names.desktop_name("zoom");
            first.get_or_insert(got.clone());
            if got.is_some() {
                assert_eq!(got.as_deref(), Some("Zoom Workplace"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the background scan never landed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            first,
            Some(None),
            "the first look answered at once, unscanned"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exec_lines_name_their_program() {
        assert_eq!(exec_program("/usr/bin/zoom %U").as_deref(), Some("zoom"));
        assert_eq!(
            exec_program("env FOO=1 -u BAR /opt/app/run --x").as_deref(),
            Some("run")
        );
        assert_eq!(
            exec_program("\"/opt/My App/app\" --flag").as_deref(),
            Some("app")
        );
        assert_eq!(exec_program(""), None);
    }

    #[test]
    fn only_running_app_captures_count_as_using() {
        let mut idle = stream("a", Some("Discord"), Some("discord"));
        idle.state = "idle".into();
        let mut meter = stream("b", Some("Volume Control"), Some("pavucontrol"));
        meter.monitor = true;
        let mut stage = stream("c", None, Some("pipewire"));
        stage.stage = true;
        let ours = stream("hushmic_input", None, None);
        let mut playback = stream("d", Some("mpv"), Some("mpv"));
        playback.media_class = "Stream/Output/Audio".into();
        let ee = stream("easyeffects_source", Some("EasyEffects"), None);
        let firefox = stream("e", Some("Firefox"), Some("firefox"));
        let firefox2 = stream("f", Some("Firefox"), Some("firefox"));
        let all = [idle, meter, stage, ours, playback, ee, firefox, firefox2];
        let got = using(&all, &none, &no_names());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].name.as_deref(), Some("Firefox"));
        // A stream of our own process tree never counts.
        assert!(using(
            &all,
            &|r: &Reader| r.binary.as_deref() == Some("firefox"),
            &no_names()
        )
        .is_empty());
    }

    #[test]
    fn a_direct_reader_needs_seconds_not_ticks_and_goes_at_once() {
        let mut w = DirectWatch::default();
        let names = no_names();
        let mut zoom = stream("z", Some("Zoom"), Some("zoom"));
        zoom.target = Some("alsa_input.rode".into());
        let chrome = stream("c", Some("Google Chrome"), Some("chrome"));
        let readers = [zoom.clone(), chrome];
        let show = |_: &str| false;
        let t0 = Instant::now();
        let at = |secs: f32| t0 + Duration::from_secs_f32(secs);
        let mut look = |rs: &[Reader], hidden: &dyn Fn(&str) -> bool, on: bool, secs: f32| {
            w.observe_at(rs, on, hidden, &none, &names, at(secs))
        };
        assert!(look(&readers, &show, true, 0.0).is_empty(), "first look");
        // Early ticks (1.5 s after a command) cannot rush it.
        assert!(look(&readers, &show, true, 1.5).is_empty(), "an early tick");
        let got = look(&readers, &show, true, 5.0);
        assert_eq!(got.len(), 2);
        assert!(!got[0].follows_default, "Zoom pinned the device");
        assert!(got[1].follows_default, "Chrome follows the default");
        // Chrome stops: it leaves at once; Zoom stays.
        assert_eq!(look(&[zoom.clone()], &show, true, 10.0).len(), 1);
        // Chrome comes back: its time starts again.
        let back = [
            zoom.clone(),
            stream("c", Some("Google Chrome"), Some("chrome")),
        ];
        assert_eq!(look(&back, &show, true, 15.0).len(), 1);
        assert_eq!(look(&back, &show, true, 20.0).len(), 2);
        // Hidden apps are left out; off clears the streaks.
        let hide_zoom = |id: &str| id == "zoom";
        assert_eq!(look(&back, &hide_zoom, true, 25.0).len(), 1);
        assert!(look(&back, &show, false, 30.0).is_empty());
        assert!(look(&back, &show, true, 35.0).is_empty(), "streak reset");
    }

    #[test]
    fn runtimes_are_matched_whole_not_by_prefix() {
        for rt in [
            "electron",
            "electron28",
            "python3",
            "python3.12",
            "java",
            "sh",
            "wine64-preloader",
        ] {
            assert!(is_runtime(rt), "{rt}");
        }
        for app in [
            "shotcut",
            "shutter",
            "shortwave",
            "monodevelop",
            "nodebox",
            "javelin",
        ] {
            assert!(!is_runtime(app), "{app}");
        }
        let names = no_names();
        let s = stream("x", Some("WEBRTC VoiceEngine"), Some("shotcut"));
        assert_eq!(app_ref(&s, &names).name.as_deref(), Some("shotcut"));
    }

    #[test]
    fn what_an_app_says_is_cleaned_and_bounded() {
        let names = no_names();
        let odd = stream("x", Some("Evil\nname = \"x\"\t\u{7}"), Some("evil\nbin"));
        let a = app_ref(&odd, &names);
        assert_eq!(a.name.as_deref(), Some("Evil name = \"x\""));
        assert_eq!(a.id, "evil_bin");
        let long = stream("x", Some(&"é".repeat(5000)), Some(&"b".repeat(5000)));
        let a = app_ref(&long, &names);
        assert_eq!(a.name.unwrap().chars().count(), MAX_NAME);
        assert_eq!(a.id.chars().count(), MAX_ID);
    }

    #[test]
    fn our_streams_are_our_recorders_not_every_child() {
        let me = std::process::id();
        let mut rec = stream("r", Some("pw-record"), Some("pw-record"));
        rec.pid = Some(me);
        assert!(is_ours(&rec), "a recorder we run");
        // A browser HushMic happened to start (a link opened from the
        // window) is the user's app.
        let mut browser = stream("b", Some("Firefox"), Some("firefox"));
        browser.pid = Some(me);
        assert!(!is_ours(&browser));
        // A recorder that is not ours is an app like any other.
        let mut theirs = stream("t", Some("pw-record"), Some("pw-record"));
        theirs.pid = Some(1);
        assert!(!is_ours(&theirs));
    }

    #[test]
    fn our_own_process_is_ours() {
        assert!(is_own_pid(std::process::id()));
        assert!(!is_own_pid(1));
        assert_eq!(
            parent_pid(std::process::id()),
            Some(unsafe { libc::getppid() } as u32)
        );
    }
}
