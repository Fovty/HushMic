use crate::config::{ActiveProfile, Config};
use crate::pipewire;
use crate::portwriter::PortWriter;
use directories::ProjectDirs;
use hushmic_shm::control::ControlWriter;
use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Filesystem locations the controller needs to spawn the filter-chain host.
///
/// `plugin_so` is the LADSPA `.so`, `model_dir` holds the `<model>.onnx`
/// files, and `dylib` is the ONNX Runtime shared object that the plugin
/// `dlopen`s via the `ORT_DYLIB_PATH` env var.
#[derive(Clone, Debug)]
pub struct Paths {
    pub plugin_so: PathBuf,
    pub model_dir: PathBuf,
    pub dylib: PathBuf,
}

/// The install prefix implied by the running binary's location: `<p>/bin/x`
/// maps to `<p>`. None for non-installed layouts (e.g. `target/release/x`).
/// Pub for testability (pure path logic).
pub fn prefix_of(exe: &std::path::Path) -> Option<PathBuf> {
    let bin_dir = exe.parent()?;
    if bin_dir.file_name()? != "bin" {
        return None;
    }
    Some(bin_dir.parent()?.to_path_buf())
}

impl Paths {
    /// Priority per path: explicit env override > install-prefix-relative
    /// (derived from the running binary's location, so `/usr/local` and
    /// `$HOME/.local` installs — and .desktop launches, which see none of the
    /// shell exports install.sh prints — work without any env) > the
    /// compiled-in /usr defaults.
    pub fn resolve() -> Self {
        let prefix = std::env::current_exe().ok().and_then(|exe| prefix_of(&exe));
        let from_prefix = |rel: &str| -> Option<PathBuf> {
            let p = prefix.as_ref()?.join(rel);
            p.exists().then_some(p)
        };
        let plugin_so = std::env::var("HUSHMIC_PLUGIN_SO")
            .map(PathBuf::from)
            .ok()
            .or_else(|| from_prefix("lib/ladspa/libdpdfnet_ladspa.so"))
            .unwrap_or_else(|| PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"));
        let model_dir = std::env::var("HUSHMIC_MODEL_DIR")
            .map(PathBuf::from)
            .ok()
            .or_else(|| from_prefix("share/hushmic/models"))
            .unwrap_or_else(|| PathBuf::from("/usr/share/hushmic/models"));
        let dylib = std::env::var("ORT_DYLIB_PATH")
            .map(PathBuf::from)
            .ok()
            .or_else(|| from_prefix("lib/hushmic/libonnxruntime.so"))
            // Distro packages (Arch) link against the SYSTEM onnxruntime and
            // ship nothing under lib/hushmic/ — fall back to the system lib
            // before giving up, or enable()'s preflight hard-fails on Arch.
            .or_else(|| {
                let p = PathBuf::from("/usr/lib/libonnxruntime.so");
                p.exists().then_some(p)
            })
            .unwrap_or_else(|| PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"));
        Paths {
            plugin_so,
            model_dir,
            dylib,
        }
    }
}

/// Parse a kernel cpulist string ("0-15", "0-3,8-11", "7") into cpu ids.
/// Pub for testability (pure parsing).
pub fn parse_cpu_list(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                if a <= b && b - a < 4096 {
                    out.extend(a..=b);
                }
            }
        } else if let Ok(v) = part.parse::<usize>() {
            out.push(v);
        }
    }
    out
}

/// Never pin down to a degenerate set: when a cgroup cpuset leaves only a
/// sliver of the P-cores allowed, confining the whole host to 1-3 shared
/// cpus is worse than letting the scheduler use the full allowed set.
const MIN_PIN_CPUS: usize = 4;

/// Takeover attempts in a row (one per healthy tick) before the daemon
/// backs off to one per [`TAKEOVER_BACKOFF`]: a session manager that
/// answers but drops the write is not fought every tick.
const TAKEOVER_TRIES: u32 = 5;

/// How often a takeover that keeps missing is tried after
/// [`TAKEOVER_TRIES`]. Never given up: while HushMic is not the default,
/// apps on the default record the raw mic.
const TAKEOVER_BACKOFF: Duration = Duration::from_secs(30);

/// What a teardown waits beyond the cap of a takeover write that is being
/// sent, for the worker to say so.
const TAKEOVER_WRITE_SLACK: Duration = Duration::from_secs(1);

/// How often Off and Quit try to give the default back while PipeWire does
/// not answer, each try under one [`pipewire::CALL_BUDGET`]. After them
/// `hushmic_source` is gone and nothing of ours puts the default right
/// until the next start, so one try that a busy session manager let pass
/// is not enough.
const RESTORE_TRIES: u32 = 3;

/// `restore`, again while it gets no answer (`ErrorKind::TimedOut`), `tries`
/// times at most; `missed` hears each try another one follows. An answer
/// ends it, whatever it says: a refusal, or a default read back that is
/// not the one written, would be the same the next time. The error comes
/// with the number of the try it ended.
fn restore_patiently(
    tries: u32,
    mut restore: impl FnMut() -> std::io::Result<()>,
    mut missed: impl FnMut(u32),
) -> Result<(), (std::io::Error, u32)> {
    let mut n = 1;
    loop {
        match restore() {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut && n < tries => {
                missed(n);
                n += 1;
            }
            Err(e) => return Err((e, n)),
        }
    }
}

/// The cpu ids in this process's CURRENT affinity mask — cgroup cpusets and
/// any deliberate taskset/CPUAffinity= restriction included. None if
/// unreadable. Sorted ascending by construction.
pub(crate) fn current_affinity() -> Option<Vec<usize>> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let rc =
        unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) };
    if rc != 0 {
        return None;
    }
    Some(
        (0..libc::CPU_SETSIZE as usize)
            .filter(|&c| unsafe { libc::CPU_ISSET(c, &set) })
            .collect(),
    )
}

/// The pin decision given the machine's P-cores and the cpus this process
/// may use: the intersection, but only when it truly narrows placement
/// (non-empty, strictly smaller than `allowed`) and is not degenerate.
/// An empty intersection means the P-cores are off-limits (cgroup cpuset,
/// or the user deliberately taskset us elsewhere) — that choice wins.
/// Pub for testability (pure set logic).
pub fn pin_intersection(pcores: &[usize], allowed: &[usize]) -> Option<Vec<usize>> {
    let target: Vec<usize> = pcores
        .iter()
        .copied()
        .filter(|c| allowed.contains(c))
        .collect();
    if target.len() < MIN_PIN_CPUS || target.len() >= allowed.len() {
        return None;
    }
    Some(target)
}

/// The cpus to confine the filter-chain host to, or None when pinning is
/// disabled, pointless, or would fight an existing restriction.
///
/// Why pin at all: the host's realtime DSP thread lands on an arbitrary
/// core at spawn. On Intel hybrid CPUs an E-core roughly DOUBLES the
/// model's per-quantum inference time (measured 13.6 ms vs ~6 ms of a 21 ms
/// budget on a 13700KF) — enough margin on an idle desktop, but any
/// cache-/bandwidth-heavy neighbor (a video call's audio engine) pushes it
/// over the deadline and the resulting xrun storm starves hushmic_source
/// for every consumer at once, which sounds like stuttering garbage.
///
/// The P-core list comes from /sys/devices/cpu_core/cpus (present only on
/// hybrid Intel, kernel 5.13+) and is intersected with our own affinity
/// mask so a cgroup cpuset or deliberate taskset is respected, never
/// widened past, and never silently shrunk into by the kernel's own
/// mask-ANDing. HUSHMIC_NO_CPU_PIN=1 disables the whole mechanism.
fn pin_target() -> Option<Vec<usize>> {
    if std::env::var_os("HUSHMIC_NO_CPU_PIN").is_some() {
        return None;
    }
    let s = std::fs::read_to_string("/sys/devices/cpu_core/cpus").ok()?;
    let pcores = parse_cpu_list(s.trim());
    if pcores.is_empty() {
        return None;
    }
    let allowed = current_affinity()?;
    let target = pin_intersection(&pcores, &allowed);
    if target.is_none() {
        // Hybrid CPU but no (sane) pin possible: say why once per spawn, or
        // "the fix didn't engage" reports are undiagnosable.
        eprintln!(
            "[hushmic] hybrid CPU detected but not pinning the filter-chain host \
             (allowed cpus: {}, performance cores among them: {})",
            allowed.len(),
            pcores.iter().filter(|c| allowed.contains(c)).count()
        );
    }
    target
}

/// Where HushMic keeps what it generates and nobody edits (the two chain
/// confs, the prior-default breadcrumb, the filter-chain log):
/// `$XDG_STATE_HOME/hushmic`, not `~/.config` (a dotfiles repo must not
/// pick them up).
pub fn state_dir() -> PathBuf {
    let d = ProjectDirs::from("io", "hushmic", "hushmic").expect("home");
    d.state_dir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| d.data_local_dir().to_path_buf())
}

/// Where versions before 1.0 wrote those files: next to the config.
fn legacy_state_dir() -> PathBuf {
    ProjectDirs::from("io", "hushmic", "hushmic")
        .expect("home")
        .config_dir()
        .to_path_buf()
}

/// How long a chain HushMic stops gets to exit on SIGTERM (it tears its
/// nodes down cleanly, so apps linked to them are moved, not left with a
/// peer that vanished) before SIGKILL.
pub const TERM_GRACE: Duration = Duration::from_millis(400);

/// How [`terminate`] ended a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// It had exited already.
    Gone,
    /// It exited on SIGTERM, after this long.
    Termed(Duration),
    /// It was still there after the grace and was killed.
    Killed,
}

/// Stop a child: SIGTERM, wait until it is gone or `grace` has passed,
/// then SIGKILL. Reaped either way, so nothing is left behind.
pub fn terminate(child: &mut Child, grace: Duration) -> Ended {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return Ended::Gone;
    }
    let started = Instant::now();
    // SAFETY: a signal to our own child, not yet reaped.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    while started.elapsed() < grace {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return Ended::Termed(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if matches!(child.try_wait(), Ok(Some(_))) {
        return Ended::Termed(started.elapsed());
    }
    let _ = child.kill();
    let _ = child.wait();
    Ended::Killed
}

/// [`terminate`] a chain or host process with [`TERM_GRACE`], saying so
/// when it had to be killed.
pub fn stop_process(child: &mut Child, what: &str) -> Ended {
    let ended = terminate(child, TERM_GRACE);
    if ended == Ended::Killed {
        eprintln!(
            "[hushmic] the {what} did not exit within {} ms of SIGTERM; killed",
            TERM_GRACE.as_millis()
        );
    }
    ended
}

/// The generated files, by name.
const STATE_FILES: [&str; 3] = ["filter-chain.conf", "source.conf", "prior-default"];

/// Move what an earlier version generated next to the config into the
/// state directory. Once per start, before anything reads them: the
/// prior-default breadcrumb may be a crashed run's, and the chain conf
/// tells an upgrader from a fresh install.
pub fn migrate_state() {
    migrate_state_in(&legacy_state_dir(), &state_dir());
}

fn migrate_state_in(old: &std::path::Path, new: &std::path::Path) {
    if old == new {
        return;
    }
    for name in STATE_FILES {
        let from = old.join(name);
        if !from.exists() {
            continue;
        }
        let to = new.join(name);
        if to.exists() {
            // the newer one is there already
            let _ = std::fs::remove_file(&from);
            continue;
        }
        let _ = std::fs::create_dir_all(new);
        // another filesystem: copy, then remove
        if std::fs::rename(&from, &to).is_err() && std::fs::copy(&from, &to).is_ok() {
            let _ = std::fs::remove_file(&from);
        }
    }
}

/// The chain conf a start writes (it also says HushMic ran here).
pub fn conf_path() -> PathBuf {
    state_dir().join("filter-chain.conf")
}

fn host_conf_path() -> PathBuf {
    state_dir().join("source.conf")
}

/// The `node.link-group` of all four nodes (the chain's capture and output,
/// the source host's feed and `hushmic_source`). The session manager never
/// links a node to one in its own group: a chain that follows the default
/// is never fed by `hushmic_source` once that is the default (a loop,
/// silence), even when its pinned mic is gone and it falls back.
pub const LINK_GROUP: &str = "hushmic";

/// The base of both `pipewire -c` confs: the core modules a bare
/// filter-chain needs (see [`render_conf`]).
pub(crate) const CONF_BASE: &str = r#"context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}
context.modules = [
  { name = libpipewire-module-rt
    args = { }
    flags = [ ifexists nofail ]
  }
  { name = libpipewire-module-protocol-native }
  { name = libpipewire-module-client-node }
  { name = libpipewire-module-adapter }
  { name = libpipewire-module-metadata }"#;

/// The source host's conf: `hushmic_source` as apps and their saved routes
/// know it (name, description, class, format, quantum pin), copied from
/// `hushmic_feed`, which only the daemon's link to the chain feeds (the
/// session manager never routes it, and nothing can move it). The host
/// outlives every chain restart. Pure function — no I/O.
pub fn render_host_conf() -> String {
    format!(
        r#"# hushmic source host: hushmic_source, fed by the DSP chain (generated; do not edit).
context.properties = {{
    log.level = 2
}}
{base}
  {{ name = libpipewire-module-filter-chain
    flags = [ nofail ]
    args = {{
      node.description = "HushMic"
      media.name       = "HushMic"
      filter.graph = {{
        nodes = [ {{ type = builtin name = hushmic_copy label = copy }} ]
      }}
      capture.props = {{
        node.name           = "{feed}"
        node.description    = "HushMic internal"
        media.name          = "HushMic internal"
        # No application.name on HushMic's streams: WirePlumber 0.4 keys
        # saved stream volumes by it before media.name (and ignores
        # restore-props), so the feed would share hushmic_input's key.
        node.autoconnect    = false
        node.dont-reconnect = true
        node.dont-move      = true
        node.passive        = true
        node.link-group     = "{group}"
        state.restore-props = false
        audio.rate     = 48000
        audio.channels = 1
        audio.position = [ MONO ]
        # Mono whatever is linked in: WirePlumber 0.4 otherwise remakes the
        # ports for a stream linked in by a target (two channels), and the
        # chain's link goes with the old one (A125 SF-1).
        stream.dont-remix = true
      }}
      playback.props = {{
        node.name        = "hushmic_source"
        node.description  = "HushMic"
        media.class      = Audio/Source
        node.link-group  = "{group}"
        audio.rate       = 48000
        audio.channels   = 1
        audio.position   = [ MONO ]
        node.force-quantum = {pin}
      }}
    }}
  }}
]
"#,
        base = CONF_BASE,
        feed = pipewire::HOST_FEED,
        group = LINK_GROUP,
        pin = PINNED_QUANTUM,
    )
}

/// Where the pre-takeover system default is persisted while `set_default` is
/// active, so a run that dies without restoring it (SIGKILL, power loss) can
/// be repaired by the next launch — see [`recover_dangling_default`].
fn prior_default_path() -> PathBuf {
    state_dir().join("prior-default")
}

/// Write the breadcrumb, and wait until it is on the disk. True when
/// there was none before this one.
pub(crate) fn persist_prior_default(name: &str) -> bool {
    let path = prior_default_path();
    let made = !path.exists();
    persist_prior_default_in(&path, name);
    made
}

/// [`persist_prior_default`] at `path`. An empty name ("no default
/// before") never replaces a known mic (UX panel A26): a takeover that
/// found nothing better must not erase the one copy of the user's device.
fn persist_prior_default_in(path: &Path, name: &str) {
    if name.trim().is_empty()
        && std::fs::read_to_string(path)
            .is_ok_and(|s| !s.trim().is_empty() && s.trim() != "hushmic_source")
    {
        return;
    }
    // Atomic: this breadcrumb exists precisely for crash recovery, so it
    // must not itself be truncatable by the crash (the empty-file case is
    // tolerated by the reader, but then the real prior default is lost).
    let _ = crate::fsutil::atomic_write(path, name.as_bytes());
}

/// A previous run's persisted prior default, if any (crash leftover). Never
/// yields our own node name.
/// The persisted pre-takeover default (the breadcrumb `enable()` writes
/// while "Set as default microphone" holds the default) — lets --doctor,
/// which runs outside the tray process, judge the capture feeder in
/// set-default configurations.
pub fn persisted_prior_default() -> Option<String> {
    read_persisted_prior_default()
}

pub(crate) fn read_persisted_prior_default() -> Option<String> {
    // an earlier version's, until a start moves it (--doctor runs apart)
    let s = std::fs::read_to_string(prior_default_path())
        .or_else(|_| std::fs::read_to_string(legacy_state_dir().join("prior-default")))
        .ok()?;
    let s = s.trim();
    if s.is_empty() || s == "hushmic_source" {
        None
    } else {
        Some(s.to_string())
    }
}

pub(crate) fn clear_persisted_prior_default() {
    let _ = std::fs::remove_file(prior_default_path());
}

/// Crash recovery, run once at startup: if a previous run repointed the system
/// default to `hushmic_source` and died before restoring it, the persisted
/// copy of the user's real default lets us put it back. If the default was
/// changed since (by the user or anything else), their choice wins and the
/// stale record is dropped.
pub fn recover_dangling_default() {
    // Where default-source writes are impossible (a Flatpak without the
    // Manager drop-in — see pipewire::can_set_default), the restore below
    // would fail anyway, and no takeover can have happened from in here
    // either (take_default is gated the same way) — any breadcrumb file is
    // another install's to fix.
    if !pipewire::can_set_default() {
        return;
    }
    let p = prior_default_path();
    let Ok(prev) = std::fs::read_to_string(&p) else {
        return;
    };
    let prev = prev.trim();
    match pipewire::get_default_source() {
        Some(cur) if cur == "hushmic_source" => {
            let restored = if prev.is_empty() {
                // there was no prior default; clear the dangling key
                pipewire::clear_default_source().is_ok()
            } else {
                pipewire::set_default_source(prev).is_ok()
            };
            if restored {
                eprintln!(
                    "[hushmic] restored the default microphone left dangling by a previous run"
                );
                let _ = std::fs::remove_file(&p);
            }
            // on failure keep the file: the next launch retries
        }
        Some(_) => {
            // default moved on without us; nothing dangling
            let _ = std::fs::remove_file(&p);
        }
        None => {
            // could be "no key set" OR "daemon unreachable" — keep the file,
            // it is harmless and a later launch can still tell the difference
        }
    }
}

/// Escape a string for embedding inside a double-quoted SPA-JSON string in the
/// rendered conf: device node names come from hardware/user config and may
/// contain quotes or backslashes, which would otherwise break the conf (or
/// inject keys into it).
pub(crate) fn spa_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Render a SELF-CONTAINED PipeWire config for `pipewire -c <conf>`.
///
/// A bare filter-chain *fragment* (`context.modules = [ filter-chain ]`)
/// fails to load standalone with
/// `can't find protocol 'PipeWire:Protocol:Native'` because it carries none of
/// the core modules. Since the controller spawns exactly `pipewire -c <conf>`,
/// this emits the base preamble (`context.properties`, `context.spa-libs`, and
/// the base `context.modules`: rt, protocol-native, client-node, adapter,
/// metadata — mirroring `/usr/share/pipewire/filter-chain.conf` and
/// `minimal.conf`) and then appends the hushmic filter-chain module.
///
/// Its output is `hushmic_dsp_out`, an internal stream the daemon links
/// into the source host ([`render_host_conf`]); the session manager never
/// routes it. The filter-chain is mono, 48 kHz, and (when a mic is chosen) pins the mic
/// via `target.object`, plus the legacy `node.target` when `legacy_node_target`
/// is set (PipeWire < 0.3.64 ignores `target.object`). With `report_latency`
/// a report-only builtin `delay` node follows the DSP so PipeWire >= 1.6
/// declares the chain's algorithmic latency to consumers (OBS A/V sync);
/// see [`LATENCY_SAMPLES`]. Pure function — no I/O.
pub fn render_conf(
    cfg: &Config,
    paths: &Paths,
    legacy_node_target: bool,
    mode: RunMode,
    report_latency: bool,
) -> String {
    // Pin the chosen mic; otherwise follow the system default capture device.
    // `target.object` is the modern key (PipeWire >= 0.3.64); older PipeWire
    // ignores it, so `legacy_node_target` additionally emits the pre-0.3.64
    // `node.target`. Only set on detected-old PipeWire, so a modern conf stays
    // byte-identical.
    let target = match &cfg.mic {
        Some(name) => {
            let esc = spa_escape(name);
            let mut t = format!("        target.object  = \"{esc}\"\n");
            if legacy_node_target {
                t.push_str(&format!("        node.target     = \"{esc}\"\n"));
            }
            t
        }
        None => String::new(),
    };
    // Belt-and-suspenders (Config::load already sanitizes): a non-finite value
    // would render as a literal `NaN`/`inf` token that PipeWire rejects.
    let attn = if cfg.attn_limit.is_finite() {
        cfg.attn_limit.clamp(0.0, 100.0)
    } else {
        100.0
    };
    // Report-only latency node: `Delay (s) = 0.0` never delays audio; the
    // `latency` config (seconds, derived from the samples constant so the
    // two cannot drift) is what filter-chain >= 1.6 folds into the
    // propagated latency. Gated: 0.3.48 has no delay builtin at all — the
    // node would kill the chain there — and < 1.6 would ignore the key.
    let (latency_node, links) = if report_latency {
        let secs = LATENCY_SAMPLES as f64 / 48_000.0;
        (
            format!(
                "\n          {{ type   = builtin\n            name   = hushmic_latency\n            label  = delay\n            config = {{ \"max-delay\" = 0.001 \"latency\" = {secs} }}\n            control = {{ \"Delay (s)\" = 0.0 }}\n          }}"
            ),
            "\n        links = [\n          { output = \"hushmic_dsp:Output\" input = \"hushmic_latency:In\" }\n        ]"
                .to_string(),
        )
    } else {
        (String::new(), String::new())
    };
    format!(
        r#"# hushmic self-contained PipeWire filter-chain host (generated; do not edit).
# Base modules mirror /usr/share/pipewire/filter-chain.conf so a bare
# `pipewire -c <this>` has the core protocol + node infrastructure; the hushmic
# filter-chain module is then appended. See crates/dpdfnet-ladspa/examples/run-filter-chain.md.
context.properties = {{
    log.level = 2
}}
{base}
  {{ name = libpipewire-module-filter-chain
    flags = [ nofail ]
    args = {{
      node.description = "HushMic"
      media.name       = "HushMic"
      filter.graph = {{
        nodes = [
          {{ type   = ladspa
            name   = hushmic_dsp
            plugin = "{plugin}"
            label  = "dpdfnet_mono"
            control = {{ "Attenuation Limit (dB)" = {attn} "Mode" = {mode} }}
          }}{latency_node}
        ]{links}
      }}
      capture.props = {{
        node.name      = "{input}"
        # Not "HushMic": that is the microphone apps pick (UX panel
        # round 2, A11). Its own media.name too, or it takes the
        # module's "HushMic", and mixers (pavucontrol names streams by
        # media.name) list a second "HushMic" (A115 SF-L1).
        node.description = "HushMic input (internal)"
        media.name       = "HushMic input (internal)"
        node.passive   = true
        node.link-group = "{group}"
        audio.rate     = 48000
        audio.channels = 1
        audio.position = [ MONO ]
{target}      }}
      playback.props = {{
        node.name           = "{output}"
        node.description    = "HushMic internal"
        media.name          = "HushMic internal"
        node.autoconnect    = false
        node.dont-reconnect = true
        node.dont-move      = true
        # Passive (and so is the link): with nobody recording HushMic the
        # chain idles and the mic is released.
        node.passive        = true
        node.link-group     = "{group}"
        # A mixer's volume or mute on this internal stream is not restored
        # (WirePlumber 0.5; on 0.4 the link worker resets it).
        state.restore-props = false
        audio.rate       = 48000
        audio.channels   = 1
        audio.position   = [ MONO ]
        # Its ports stay as they are when something records it (see the
        # host's feed).
        stream.dont-remix = true
        # Pin the graph quantum while the chain runs (issue #10). The DSP
        # runs on its own worker thread, so any quantum meets the RT
        # deadline; the pin's job is to keep the quantum KNOWN and SMALL,
        # making the async output margin (and the declared latency) exact
        # and minimal. One 10 ms hop = WebRTC's native request. Hosts
        # without the property ignore it. The pw-metadata
        # clock.force-quantum setting remains the manual override (values
        # above the pin raise real latency beyond the declared figure).
        node.force-quantum = {pin}
      }}
    }}
  }}
]
"#,
        base = CONF_BASE,
        input = pipewire::CHAIN_INPUT,
        output = pipewire::CHAIN_OUTPUT,
        group = LINK_GROUP,
        plugin = spa_escape(&paths.plugin_so.display().to_string()),
        attn = attn,
        mode = mode.control_value(),
        target = target,
        latency_node = latency_node,
        links = links,
        pin = PINNED_QUANTUM,
    )
}

/// The chain's algorithmic latency in samples at 48 kHz: 2400 engine
/// latency (480 STFT framing + 1920 model group delay — MEASURED: the
/// hushmic-denoiser latency tests push impulses and real speech through
/// the actual DSP and pin it exactly) + 1440 async output lead (one
/// pinned 480-sample quantum + 960 samples / 20 ms of worker stall
/// headroom, since inference runs on its own thread — issue #10)
/// = 3840 = 80 ms. The plugin pins the same figure as
/// PLUGIN_LATENCY_SAMPLES (crates/dpdfnet-ladspa/src/align.rs) and its
/// asset-gated test measures the whole plugin end to end; change either
/// side and a test forces this constant to be re-derived. PipeWire adds
/// its own quantum/device buffering on top.
/// The light model's id: the plugin's fallback tier under CPU pressure.
pub const LIGHT_MODEL: &str = "dpdfnet2_48khz_hr";

/// The config a chain start runs with: the saved one, or during a mic
/// recovery fallback the same with the mic cleared, so the chain follows
/// the system default. The fallback was decided on debounced evidence
/// that the saved mic is gone; a later start whose device read fails (a
/// failed read keeps the saved mic: unknown is not gone) or is stale must
/// not pin the chain to it again, which would read as "on the preferred
/// mic" and stop the return. Pure.
pub fn restart_config(cfg: &Config, mic_fallback: bool) -> Config {
    if mic_fallback {
        Config {
            mic: None,
            ..cfg.clone()
        }
    } else {
        cfg.clone()
    }
}

/// Whether a settings change restarts the chain: when its values change,
/// or when an explicit mic pick ended a recovery fallback (the saved mic
/// is the same, but the chain still followed the default). Pure.
pub fn settings_restart(values_changed: bool, fallback_ended: bool) -> bool {
    values_changed || fallback_ended
}

/// The variable that turns the plugin's voice hold on, and what it is
/// spawned with for the setting: nothing while off, so a variable the
/// user started HushMic with is inherited as before.
pub const VOICE_HOLD_ENV: &str = "HUSHMIC_VOICE_HOLD";
pub fn voice_hold_env(on: bool) -> &'static [(&'static str, &'static str)] {
    if on {
        &[(VOICE_HOLD_ENV, "1")]
    } else {
        &[]
    }
}

/// What the chain's `HUSHMIC_INFERENCE` should be set to for the
/// configured engine choice (issue #18): only `onnx` needs saying, since
/// the plugin's default is auto. A value already in our own environment is
/// a debugging override and is inherited unchanged.
pub fn plugin_inference(
    setting: crate::config::Inference,
    inherited: bool,
) -> Option<&'static str> {
    (setting == crate::config::Inference::Onnx && !inherited).then_some("onnx")
}

/// What a running child was rendered with: the part of the config that
/// reaches its conf or its spawn, resolved for the device its settings
/// came from. [`Controller::apply_settings`] diffs a new config against it
/// to choose between a live control write and a restart.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// The mic the config named (after a recovery fallback cleared it),
    /// NOT the resolved one: a saved mic that was absent at spawn is the
    /// recovery machine's business, not a reason to restart on every
    /// settings change while it is unplugged.
    pub mic_pref: Option<String>,
    /// The effective model and strength for the device in use.
    pub model: String,
    pub attn: f32,
    pub set_default: bool,
    pub inference: crate::config::Inference,
    /// The plugin's voice hold: read when it is instantiated.
    pub voice_hold: bool,
}

impl Rendered {
    /// What `cfg` renders as for a chain whose settings come from
    /// `profile_device`. Pure: the one place both `enable()` and
    /// `apply_settings()` derive the rendered values from.
    pub fn of(cfg: &Config, profile_device: Option<&str>) -> Rendered {
        let profile = cfg.profile_for(profile_device);
        Rendered {
            mic_pref: cfg.mic.clone(),
            model: profile.model,
            attn: profile.attn_limit,
            set_default: cfg.set_default,
            inference: cfg.inference,
            voice_hold: cfg.voice_hold,
        }
    }
}

/// What separates the running chain from a wanted config, and how to
/// close the gap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChainDelta {
    /// Nothing the child was rendered with changed.
    Unchanged,
    /// Only the strength moved: a control-port write on the running node
    /// (the plugin reads it every cycle).
    Attn(f32),
    /// Something baked into the conf or the spawn changed (mic, model,
    /// which loads at instantiate, default takeover, engine), or there is
    /// no running child at all.
    Restart,
}

impl ChainDelta {
    /// `running` = what the live child was rendered with (None: no live
    /// child), `wanted` = the same for the new config.
    pub fn of(running: Option<&Rendered>, wanted: &Rendered) -> ChainDelta {
        let Some(r) = running else {
            return ChainDelta::Restart;
        };
        if r.mic_pref != wanted.mic_pref
            || r.model != wanted.model
            || r.set_default != wanted.set_default
            || r.inference != wanted.inference
            || r.voice_hold != wanted.voice_hold
        {
            ChainDelta::Restart
        } else if r.attn != wanted.attn {
            ChainDelta::Attn(wanted.attn)
        } else {
            ChainDelta::Unchanged
        }
    }

    pub fn restarts(self) -> bool {
        self == ChainDelta::Restart
    }
}

/// What [`Controller::apply_settings`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The running chain already matched.
    Unchanged,
    /// A control on the running node changed; the nodes survived.
    Live,
    /// The child was (re)spawned.
    Restarted,
}

pub const LATENCY_SAMPLES: u32 = 3840;

/// The graph quantum the chain pins while it runs (issue #10). With
/// inference decoupled onto a worker thread the RT callback is a memcpy
/// — any quantum meets its deadline — so the pin no longer buys compute
/// headroom: its job is to make the quantum KNOWN (the async output
/// margin and the declared latency are sized for exactly this value)
/// and SMALL (one 10 ms hop keeps the margin minimal). The conf
/// template renders this value as `node.force-quantum` on the source
/// node; the doctor reports whether the pin is live and warns when the
/// system-wide metadata override exceeds it (audio stays clean via the
/// plugin's adaptive lead, but real latency then exceeds the declared
/// figure).
pub const PINNED_QUANTUM: u32 = 480;

/// Runtime processing mode of the chain-alive states. Ephemeral by design:
/// never serialized to config — a muted mic must not survive into the next
/// login unnoticed. Values mirror the plugin's "Mode" control port.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RunMode {
    #[default]
    Suppress,
    Bypass,
    Mute,
}

impl RunMode {
    /// The plugin's "Mode" control port value.
    pub fn control_value(&self) -> u8 {
        match self {
            RunMode::Suppress => 0,
            RunMode::Bypass => 1,
            RunMode::Mute => 2,
        }
    }
}

/// A chain's control file and the path it was created at.
#[doc(hidden)]
pub struct ChainControl {
    pub path: PathBuf,
    pub writer: ControlWriter,
}

/// What a filter-chain process starts with: its conf, its model (and
/// with it the light fallback) and engine choice, the mode and strength
/// its control file opens with, and extra plugin environment.
#[doc(hidden)]
pub struct SpawnSpec<'a> {
    pub chain: crate::diagnostics::Chain,
    pub conf: &'a Path,
    pub model: &'a Path,
    /// The model's id: a chain already on the light model gets no fallback.
    pub model_id: &'a str,
    pub inference: crate::config::Inference,
    pub mode: u32,
    pub attn_db: f32,
    pub env: &'a [(&'a str, &'a str)],
}

/// A started filter-chain process and what goes with it, owned by the
/// caller from here on.
#[doc(hidden)]
pub struct SpawnedChain {
    pub child: Child,
    /// Its stderr tee (join it after the child is gone).
    pub tee: Option<std::thread::JoinHandle<()>>,
    pub meter: Option<PathBuf>,
    pub control: Option<ChainControl>,
    /// The engine generation it reports under.
    pub generation: u64,
}

/// Spawn `pipewire -c <conf>` as one of HushMic's chains: the plugin's
/// environment, the light fallback, the engine meter and control file of
/// a fresh generation of `spec.chain`, the stderr tee into that chain's
/// log, the parent-death signal and the P-core pin.
///
/// MUST be called from the main thread (see [`Controller::enable`]: the
/// parent-death signal is thread-scoped).
#[doc(hidden)]
pub fn spawn_chain(paths: &Paths, spec: &SpawnSpec) -> std::io::Result<SpawnedChain> {
    // Dedicated filter-chain host; env propagates to the plugin's dlopen.
    let mut command = Command::new("pipewire");
    command
        .arg("-c")
        .arg(spec.conf)
        .env("HUSHMIC_MODEL_PATH", spec.model)
        .env("ORT_DYLIB_PATH", &paths.dylib);
    for (k, v) in spec.env {
        command.env(k, v);
    }
    let inherited = std::env::var_os("HUSHMIC_INFERENCE").is_some_and(|v| !v.is_empty());
    if let Some(word) = plugin_inference(spec.inference, inherited) {
        command.env("HUSHMIC_INFERENCE", word);
    }
    // The light model is the plugin's fallback tier under CPU pressure
    // (issue #14); a chain already on it, or a stripped install, gets no
    // fallback model and the plugin degrades to raw audio instead.
    let light = paths.model_dir.join(format!("{LIGHT_MODEL}.onnx"));
    if spec.model_id != LIGHT_MODEL && light.exists() {
        command.env("HUSHMIC_FALLBACK_MODEL_PATH", &light);
    }
    // Every chain reports its engine tier under its own generation:
    // the previous chain's tee may still be draining, and its last
    // line must not land on top of this one's (see
    // diagnostics::new_engine_generation).
    let generation = crate::diagnostics::new_engine_generation_of(spec.chain);
    // The engine meter (and the Mic check tap next to it) of this chain:
    // one file per chain, so a window never reads a dead chain's levels.
    let meter = crate::meter::dir()
        .filter(|d| crate::meter::prepare_dir(d).is_ok())
        .map(|d| crate::meter::path_for(&d, spec.chain, generation));
    if let Some(m) = &meter {
        command.env(hushmic_shm::METER_ENV, m);
    }
    // Its control file, with what the conf carries: mode and strength
    // reach the plugin through it without PipeWire. Without one (no
    // runtime dir, a stale file in the way) the ports alone carry them.
    let control = meter.as_deref().and_then(|m| {
        let path = crate::meter::control_path_for(m.parent()?, spec.chain, generation);
        let attn = pipewire::attn_sanitized(spec.attn_db);
        match ControlWriter::create(&path, spec.mode, attn) {
            Ok(writer) => Some(ChainControl { path, writer }),
            Err(e) => {
                eprintln!(
                    "[hushmic] no control file for this chain ({e}); mode and strength go \
                     through PipeWire only"
                );
                None
            }
        }
    });
    if let Some(c) = &control {
        command.env(hushmic_shm::CONTROL_ENV, &c.path);
    }
    // Bind the host's lifetime to ours: if hushmic dies ungracefully
    // (crash, SIGKILL, session logout) Drop never runs, so without this the
    // child would linger and keep advertising a dead `hushmic_source` as the
    // default mic. PR_SET_PDEATHSIG makes the kernel SIGTERM the child when
    // the spawning (main) thread exits, guaranteeing teardown.
    //
    // INVARIANT: PR_SET_PDEATHSIG is THREAD-scoped — it ties the child to the
    // thread that calls prctl, not to the process. This is correct ONLY
    // because `spawn_chain()` is only called from the main thread (see the
    // `Controller::enable()` doc comment); were it called from a transient worker thread,
    // the child would be reaped when that worker exits, not on process exit.
    //
    // On Intel hybrid CPUs the whole host is additionally pinned to the
    // performance cores (see `pin_target` for the failure mode a random
    // E-core placement causes). Best-effort: a failed pin must not
    // break the mic. Both prctl and sched_setaffinity are plain
    // syscalls, safe in the fork/exec window.
    let pcores = pin_target();
    if let Some(cores) = &pcores {
        eprintln!(
            "[hushmic] pinning the filter-chain host to {} performance cores",
            cores.len()
        );
    }
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(cores) = &pcores {
                let mut set: libc::cpu_set_t = std::mem::zeroed();
                libc::CPU_ZERO(&mut set);
                for &c in cores {
                    if c < libc::CPU_SETSIZE as usize {
                        libc::CPU_SET(c, &mut set);
                    }
                }
                let _ = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
            }
            Ok(())
        });
    }
    // Pipe the child's stderr through the diagnostics tee: every line
    // still reaches our own stderr, and the persisted copy is what
    // `--doctor` / "Copy diagnostics" include (also post-mortem).
    command.stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Some(c) = control {
                c.writer.unlink_if_at(&c.path);
            }
            return Err(e);
        }
    };
    let tee = child.stderr.take().map(|stderr| {
        crate::diagnostics::spawn_stderr_tee(
            stderr,
            crate::diagnostics::log_path_of(spec.chain),
            spec.chain,
            generation,
        )
    });
    Ok(SpawnedChain {
        child,
        tee,
        meter,
        control,
        generation,
    })
}

/// Owns the `pipewire -c` child that hosts the virtual mic, plus the prior
/// system default source so it can be restored on teardown.
pub struct Controller {
    paths: Paths,
    child: Option<Child>,
    prior_default: Option<String>,
    /// The device a chain that follows the default captures from, as the
    /// last graph read saw it: the default it follows, or with that one
    /// gone, the device on its capture link (UX panel A2). None while
    /// pinned, without a chain, or unknown.
    in_use: Option<String>,
    /// True when this run repointed the system default to `hushmic_source`, so
    /// `disable` knows it must restore the prior default (if any) or otherwise
    /// clear the now-dangling key. Distinguishes "we set it" from "prior_default
    /// happened to be None", which a bare `Option` could not. Stays true when a
    /// restore attempt FAILS (daemon flap), so the prior default is retried
    /// later instead of being dropped.
    set_default_active: bool,
    /// The user moved the default away from our node (see
    /// `release_default`): no takeover until `reclaim_default`.
    default_released: bool,
    /// The takeover's write was confirmed (read back) — false while
    /// `set_default_active` only records an attempt that did not land.
    default_landed: bool,
    /// Takeover attempts in a row that did not complete (the current
    /// default unreadable, or the write not landing: a busy session
    /// manager); retried every tick up to [`TAKEOVER_TRIES`], then every
    /// [`TAKEOVER_BACKOFF`].
    takeover_misses: u32,
    /// When the next attempt is due once the misses reached
    /// [`TAKEOVER_TRIES`].
    takeover_next: Option<Instant>,
    /// The last miss was a session manager that did not answer (not one
    /// that answered and dropped the write): its next answer starts the
    /// count over ([`Controller::session_manager_answered`]).
    takeover_unanswered: bool,
    /// Runs the takeover's PipeWire calls, off the main loop; spawned at
    /// the first attempt.
    takeover: Option<crate::takeover::Worker>,
    /// The caps its attempts run under.
    takeover_caps: crate::takeover::Caps,
    /// Called by its attempts right before the breadcrumb is written
    /// (tests).
    takeover_hold: Option<crate::takeover::Hold>,
    /// Told when an attempt finished (the main loop then calls
    /// [`Controller::takeover_finished`]).
    on_takeover_done: Option<Box<dyn Fn() + Send>>,
    spawned_at: Option<Instant>,
    /// The mic the running child's conf pins (None = follows the system
    /// default). Set only once a child actually spawned; cleared by
    /// `disable()` — see [`Controller::active_mic`].
    active_mic: Option<String>,
    /// Whose settings the running child's conf carries (the per-mic
    /// profile in effect, or the globals) and their values. Set on spawn,
    /// cleared by `disable()`.
    active_profile: Option<ActiveProfile>,
    /// The profile last announced in the log, so a restart onto the same
    /// settings stays quiet.
    logged_profile: Option<ActiveProfile>,
    /// The processing mode every spawn renders into the conf. Living here
    /// (not per-`enable` argument) is what makes the mode survive automatic
    /// restarts — mic recovery re-enabling the chain must never silently
    /// unmute the mic.
    mode: RunMode,
    /// The child's stderr tee; joined in `disable()` so a draining old tee
    /// can never overwrite the engine tier of a freshly spawned chain.
    tee: Option<std::thread::JoinHandle<()>>,
    /// Mic recovery fell back from the saved mic (it vanished): every
    /// restart follows the system default until the return or a new pick
    /// ([`restart_config`]).
    mic_fallback: bool,
    /// What the running child was rendered with; set on spawn, cleared by
    /// `disable()`. `apply_settings` diffs against it.
    rendered: Option<Rendered>,
    /// The running chain's engine meter file (`HUSHMIC_METER`), removed
    /// with the chain.
    meter: Option<PathBuf>,
    /// The running chain's control file (`HUSHMIC_CONTROL`): mode and
    /// strength reach its plugin through it within a cycle; removed with
    /// the chain.
    control: Option<ChainControl>,
    /// The generation the running chain was spawned under.
    chain_generation: Option<u64>,
    /// The source host (`hushmic_source`): spawned on a start, kept through
    /// every chain restart, stopped by `disable()`.
    host: Option<Child>,
    /// When the host was spawned (its startup grace).
    host_spawned_at: Option<Instant>,
    /// The running host's generation: the takeover belongs to it, so a
    /// chain restart leaves the default and the takeover alone.
    host_generation: Option<u64>,
    /// The last host generation handed out.
    host_seq: u64,
    /// Links the chain into the host, off the main loop; spawned at the
    /// first chain start.
    link: Option<crate::link::Worker>,
    /// Told each link job's outcome (the main loop).
    on_link_done: Option<Box<dyn Fn(crate::link::Done) + Send>>,
    /// Writes the chain's ports too, off the main loop; spawned at the
    /// first live write.
    ports: Option<PortWriter>,
    /// Told the chain generation of a port write that did not land.
    on_port_failure: Option<crate::portwriter::OnFailure>,
    /// The running chain's generation for the port writer (0: none): it
    /// drops wishes for a chain that has been replaced.
    ports_current: crate::portwriter::Current,
    /// Microphone descriptions seen this run (node name to description):
    /// an unplugged microphone keeps its name, and same-name microphones
    /// keep their numbers ([`crate::snapshot::mic_names`]).
    seen: HashMap<String, String>,
}

/// What [`follow_target`] decides from.
struct FollowFacts<'a> {
    /// The pre-takeover default (what "System default" means while
    /// hushmic_source is the default).
    prior: Option<&'a str>,
    /// The device the chain captured from at the last graph read, while it
    /// followed the default ([`Controller::in_use`]).
    in_use: Option<&'a str>,
    /// This run made hushmic_source the default.
    takeover: bool,
    /// The pinned mic the chain starts on (None: it follows the default).
    effective_mic: Option<&'a str>,
    /// The device [`pipewire::resolve_chain_target`] gives the settings to.
    resolved: Option<&'a str>,
    /// The snapshot (None: the probe failed).
    sources: Option<&'a [pipewire::Source]>,
}

/// For a chain about to start: the device to pin it to while it follows
/// the default under the takeover, and the device whose settings it runs
/// with. Pure.
///
/// A chain that follows the default while hushmic_source IS the default is
/// pinned to the device it follows: unpinned, it would get the session
/// manager's fallback (the link group keeps it off hushmic_source) until
/// the tick's re-pin. Not the active mic: a release must let the re-pin
/// follow the user's new pick. Following a default that is gone (the
/// pre-takeover device was unplugged), the chain is on the device the
/// session manager fell back to, last seen on its capture link: it keeps
/// that one and runs with its settings (UX panel A2).
fn follow_target(f: &FollowFacts) -> (Option<String>, Option<String>) {
    let present = |d: &str| f.sources.is_some_and(|v| v.iter().any(|s| s.name == d));
    let fell_back = f
        .in_use
        .filter(|d| {
            f.effective_mic.is_none()
                && f.resolved.is_none()
                && crate::config::can_own_profile(d)
                && present(d)
        })
        .map(str::to_string);
    let pin = f
        .prior
        .filter(|p| present(p))
        .map(str::to_string)
        .or_else(|| fell_back.clone())
        .filter(|_| f.effective_mic.is_none() && f.takeover);
    (pin, f.resolved.map(str::to_string).or(fell_back))
}

impl Controller {
    pub fn new(paths: Paths) -> Self {
        Controller {
            paths,
            child: None,
            prior_default: None,
            in_use: None,
            set_default_active: false,
            default_released: false,
            default_landed: false,
            takeover_misses: 0,
            takeover_next: None,
            takeover_unanswered: false,
            takeover: None,
            takeover_caps: Default::default(),
            takeover_hold: None,
            on_takeover_done: None,
            spawned_at: None,
            active_mic: None,
            mode: RunMode::default(),
            tee: None,
            active_profile: None,
            logged_profile: None,
            mic_fallback: false,
            rendered: None,
            meter: None,
            control: None,
            chain_generation: None,
            host: None,
            host_spawned_at: None,
            host_generation: None,
            host_seq: 0,
            link: None,
            on_link_done: None,
            ports: None,
            on_port_failure: None,
            ports_current: Default::default(),
            seen: HashMap::new(),
        }
    }

    /// The caps the takeover's attempts run under, before the first one
    /// (tests keep their hanging stand-ins short).
    #[doc(hidden)]
    pub fn set_takeover_caps(&mut self, caps: crate::takeover::Caps) {
        self.takeover_caps = caps;
    }

    /// Called on the takeover worker right before an attempt writes the
    /// breadcrumb, set before the first one (tests stand in for a disk
    /// that is slow to sync).
    #[doc(hidden)]
    pub fn before_breadcrumb(&mut self, f: crate::takeover::Hold) {
        self.takeover_hold = Some(f);
    }

    /// Where a finished takeover attempt is announced (the main loop, which
    /// then calls [`Self::takeover_finished`]). Set before the first one.
    pub fn on_takeover_done(&mut self, f: Box<dyn Fn() + Send>) {
        self.on_takeover_done = Some(f);
    }

    /// Where each link job's outcome is told (the main loop). Set before
    /// the first start.
    pub fn on_link_done(&mut self, f: Box<dyn Fn(crate::link::Done) + Send>) {
        self.on_link_done = Some(f);
    }

    /// Where a port write that did not land is reported (the main loop):
    /// with the chain generation it was for. Set before the first live
    /// write.
    pub fn on_port_failure(&mut self, f: crate::portwriter::OnFailure) {
        self.on_port_failure = Some(f);
    }

    /// Hand the running chain the mode set with [`Self::set_mode_state`]:
    /// through its control file at once, and to its Mode port from the
    /// port writer (best effort, off this thread). False when no chain
    /// runs.
    pub fn write_mode_live(&mut self) -> bool {
        if !self.is_running() {
            return false;
        }
        let code = self.mode.control_value();
        if let Some(c) = &self.control {
            // Always a valid mode: the writer cannot refuse it.
            let stored = c.writer.set_mode(code as u32);
            debug_assert!(stored);
        }
        self.queue_ports(Some(code), None);
        true
    }

    /// The strength counterpart of [`Self::write_mode_live`].
    fn write_attn_live(&mut self, db: f32) -> bool {
        if !self.is_running() {
            return false;
        }
        let db = pipewire::attn_sanitized(db);
        if let Some(c) = &self.control {
            // Sanitized above: the writer cannot refuse it.
            let stored = c.writer.set_attn_db(db);
            debug_assert!(stored);
        }
        self.queue_ports(None, Some(db));
        true
    }

    /// Queue the running chain's current mode and strength for its ports
    /// again (after a write that failed while the chain was starting).
    pub fn rewrite_ports(&mut self) {
        if !self.is_running() {
            return;
        }
        let attn = self.rendered.as_ref().map(|r| r.attn);
        self.queue_ports(Some(self.mode.control_value()), attn);
    }

    fn queue_ports(&mut self, mode: Option<u8>, attn_db: Option<f32>) {
        let Some(generation) = self.chain_generation else {
            return;
        };
        let on_failure = &mut self.on_port_failure;
        let current = self.ports_current.clone();
        let ports = self.ports.get_or_insert_with(|| {
            PortWriter::spawn(
                on_failure.take().unwrap_or_else(|| {
                    Box::new(|generation| {
                        eprintln!("[hushmic] a port write for chain {generation} did not land");
                    })
                }),
                current,
            )
        });
        ports.write(crate::portwriter::Want {
            generation,
            mode,
            attn_db,
        });
    }

    /// Whether the running chain's plugin takes mode and strength from its
    /// control file (an older plugin, or one that cannot map it, follows
    /// only its ports).
    pub fn control_honoured(&self) -> bool {
        self.control.as_ref().is_some_and(|c| c.writer.honoured())
    }

    /// The running chain's control file: Some(whether its plugin mapped
    /// it), None without one (`hushmic status --json`: `chain.control`).
    pub fn control_file(&self) -> Option<bool> {
        self.control.as_ref().map(|c| c.writer.honoured())
    }

    /// What the running chain needs after a port write for it did not
    /// land ([`crate::portwriter::missed`]). `graph_running`: the newest
    /// graph read shows `hushmic_source` running, which any plugin, also
    /// one without a meter, only does once it has started.
    pub fn port_missed(&self, graph_running: bool, starting: bool) -> crate::portwriter::Missed {
        let ran = graph_running
            || self
                .meter
                .as_deref()
                .is_some_and(crate::meter::has_published);
        crate::portwriter::missed(
            self.control_honoured(),
            self.control.is_some(),
            ran,
            starting,
        )
    }

    /// The generation the running chain was spawned under.
    pub fn chain_generation(&self) -> Option<u64> {
        self.chain_generation
    }

    /// Remembers the descriptions of `mics` (real microphones) for the
    /// rest of the run; a device keeps the first one seen.
    pub fn note_mics(&mut self, mics: &[pipewire::Source]) {
        for m in mics {
            if !self.seen.contains_key(&m.name) {
                self.seen.insert(m.name.clone(), m.description.clone());
            }
        }
    }

    /// The microphone descriptions seen this run.
    pub fn seen_mics(&self) -> &HashMap<String, String> {
        &self.seen
    }

    /// `device`'s name as the window shows it (same-name microphones
    /// numbered), with `mics` the microphones plugged in now; None for a
    /// device that is none of the microphones known this run.
    pub fn mic_name(
        &self,
        cfg: &Config,
        mics: &[pipewire::Source],
        device: &str,
    ) -> Option<String> {
        crate::snapshot::mic_names(cfg, mics, &self.seen).remove(device)
    }

    /// The decision [`Self::apply_settings`] would take for `cfg`: pure (no
    /// PipeWire call), so a caller can invalidate what a restart destroys
    /// before the restart happens.
    pub fn delta(&mut self, cfg: &Config) -> ChainDelta {
        let running = if self.is_running() {
            self.rendered.as_ref()
        } else {
            None
        };
        let device = self.active_profile.as_ref().and_then(|p| p.device.clone());
        let wanted = Rendered::of(&restart_config(cfg, self.mic_fallback), device.as_deref());
        ChainDelta::of(running, &wanted)
    }

    /// Bring the running chain to `cfg` with the smallest change: nothing
    /// when it already matches; a live strength write when only the
    /// strength moved (control file at once, port off the loop; a restart
    /// when no chain runs); a restart for everything else. Main thread
    /// only, like [`Self::enable`].
    pub fn apply_settings(&mut self, cfg: &Config) -> std::io::Result<Applied> {
        match self.delta(cfg) {
            ChainDelta::Unchanged => Ok(Applied::Unchanged),
            ChainDelta::Attn(db) => {
                if self.write_attn_live(db) {
                    if let Some(r) = &mut self.rendered {
                        r.attn = db;
                    }
                    if let Some(p) = &mut self.active_profile {
                        p.attn_limit = db;
                    }
                    eprintln!(
                        "[hushmic] strength set to {} dB on the running chain",
                        pipewire::attn_value(db)
                    );
                    Ok(Applied::Live)
                } else {
                    self.enable(cfg)?;
                    Ok(Applied::Restarted)
                }
            }
            ChainDelta::Restart => {
                self.enable(cfg)?;
                Ok(Applied::Restarted)
            }
        }
    }

    /// The running chain's engine meter file (None while no chain runs).
    /// A new path means a new chain.
    pub fn meter_path(&self) -> Option<&Path> {
        self.meter.as_deref()
    }

    /// Enter (mic recovery's fallback) or leave (its return) the state in
    /// which every restart follows the system default instead of the saved
    /// mic.
    pub fn set_mic_fallback(&mut self, on: bool) {
        self.mic_fallback = on;
    }

    /// The user picked a mic (the tray, or `config set mic`, even to the
    /// same value) or turned HushMic on: a recovery fallback ends. True when one was latched, so
    /// the pick has to restart the chain even if nothing else changed.
    pub fn end_mic_fallback(&mut self) -> bool {
        std::mem::take(&mut self.mic_fallback)
    }

    pub fn mode(&self) -> RunMode {
        self.mode
    }

    /// Record the mode future spawns render into the conf. Does NOT touch a
    /// running child — [`Self::write_mode_live`] (or a restart) does that.
    pub fn set_mode_state(&mut self, mode: RunMode) {
        self.mode = mode;
    }

    /// The mic the RUNNING chain was rendered with (None = following the
    /// system default, or no child at all). This is the state the mic-
    /// recovery machine compares against `config.mic`: `Some` only while a
    /// live child's conf pins that device.
    pub fn active_mic(&self) -> Option<&str> {
        self.active_mic.as_deref()
    }

    /// The model the running chain was started with (per-mic profiles can
    /// differ from the global setting); None without a child.
    pub fn active_model(&self) -> Option<&str> {
        self.active_profile.as_ref().map(|p| p.model.as_str())
    }

    /// The running chain is on the light model by configuration, so a
    /// reported `light` tier is not a degradation.
    pub fn active_model_is_light(&self) -> bool {
        self.active_model() == Some(LIGHT_MODEL)
    }

    /// Whose settings the running chain carries; None without a child.
    pub fn active_profile(&self) -> Option<&ActiveProfile> {
        self.active_profile.as_ref()
    }

    /// Whether the running chain follows the default source (not pinned
    /// to a mic).
    pub fn follows_default(&self) -> bool {
        self.active_profile.is_some() && self.active_mic.is_none()
    }

    /// What a graph read says the chain captures from (`device` is the
    /// default it follows, else the device on its capture link); kept
    /// only while the chain follows the default.
    pub fn observe_in_use(&mut self, device: Option<String>) {
        self.in_use = device.filter(|_| self.follows_default());
    }

    /// A recovery fallback is about to start a chain that follows the
    /// default: `device` (where the session manager already moved the
    /// capture) stands in, so the start runs with its settings at once
    /// instead of a second restart for them (UX panel round 2, A9).
    pub fn stand_in(&mut self, device: Option<String>) {
        self.in_use = device;
    }

    /// The device a chain that follows the default captures from now,
    /// when known (see [`Self::observe_in_use`]).
    pub fn in_use(&self) -> Option<&str> {
        self.in_use.as_deref().filter(|_| self.follows_default())
    }

    /// Before a settings edit: a chain that follows the default and
    /// captures from another device than its settings came from (the
    /// default it followed is gone, or moved and the follow has not acted
    /// yet) carries that device's settings from now on, so the edit is
    /// saved for the device in use and [`Self::apply_settings`] brings the
    /// chain to them (UX panel A2). True when it moved: the chain must be
    /// brought to them even when the edit changes nothing.
    pub fn settle_in_use(&mut self, cfg: &Config) -> bool {
        let Some(device) = self.in_use().map(str::to_string) else {
            return false;
        };
        if self
            .active_profile
            .as_ref()
            .and_then(|p| p.device.as_deref())
            == Some(&device)
        {
            return false;
        }
        self.retarget_profile(cfg.profile_for(Some(&device)), None);
        true
    }

    /// The default source changed under a follow-default chain to a device
    /// whose settings equal the running ones: no restart needed, but the
    /// settings now belong to that device (the tray label, `status` and
    /// the next edit follow it). No-op without a child.
    pub fn retarget_profile(&mut self, profile: ActiveProfile, name: Option<&str>) {
        if self.active_profile.is_some() {
            self.log_profile(&profile, name);
            self.active_profile = Some(profile);
        }
    }

    fn log_profile(&mut self, profile: &ActiveProfile, name: Option<&str>) {
        if self.logged_profile.as_ref() != Some(profile) {
            eprintln!("[hushmic] settings: {}", profile.describe(name));
            self.logged_profile = Some(profile.clone());
        }
    }

    /// The default source remembered before "Set as default microphone"
    /// took over — what the chain actually follows while our own node is
    /// the default, and therefore the capture-repin expectation there.
    pub fn prior_default(&self) -> Option<&str> {
        self.prior_default.as_deref()
    }

    /// This run made `hushmic_source` the system default (and restores
    /// the prior one on the next disable).
    pub fn default_taken(&self) -> bool {
        self.set_default_active
    }

    /// The takeover's write was read back: only then can the user have
    /// moved the default away from us (a write still pending or lost is
    /// retried instead).
    pub fn default_landed(&self) -> bool {
        self.default_landed
    }

    /// The user switched the system default away from `hushmic_source`
    /// while this run held it: their pick sticks. The takeover ends
    /// without restoring anything (the default already is what the user
    /// wants), and no restart or quit takes it back until
    /// [`Self::reclaim_default`] — the next deliberate start or toggle.
    /// True when HushMic really had been the default (worth telling the
    /// user); false for a takeover whose write never landed.
    pub fn release_default(&mut self, to: &str) -> bool {
        if !self.set_default_active {
            return false;
        }
        let landed = self.default_landed;
        if landed {
            eprintln!(
                "[hushmic] the default microphone was switched to {to}; leaving it \
                 there until HushMic is turned on again"
            );
        } else {
            eprintln!(
                "[hushmic] the default microphone is {to}, not HushMic (the takeover \
                 did not take); leaving it there until HushMic is turned on again"
            );
        }
        self.set_default_active = false;
        self.default_landed = false;
        self.takeover_reset();
        self.prior_default = None;
        clear_persisted_prior_default();
        self.default_released = true;
        landed
    }

    /// A deliberate start, mode-on or "Set as default microphone" change:
    /// `set_default` may take the default again.
    pub fn reclaim_default(&mut self) {
        self.default_released = false;
        self.takeover_reset();
    }

    /// The session manager answers again (a graph read came back after
    /// one that failed or timed out). A takeover that missed because it
    /// did not answer is due at once; the count of misses stays, so a
    /// session manager that keeps answering graph reads but not the
    /// metadata still ends in the backoff. One that answered and dropped
    /// the write keeps its backoff as it is.
    pub fn session_manager_answered(&mut self) {
        if self.takeover_unanswered {
            self.takeover_next = None;
            self.takeover_unanswered = false;
        }
    }

    fn takeover_reset(&mut self) {
        self.takeover_misses = 0;
        self.takeover_next = None;
        self.takeover_unanswered = false;
    }

    /// Seconds since the current child was spawned (None = no child). The
    /// watchdog/status use this as a startup grace period: a freshly spawned
    /// host needs a moment before `hushmic_source` registers, and judging it
    /// "down" in that window causes needless kill/respawn churn.
    pub fn secs_since_spawn(&self) -> Option<u64> {
        self.spawned_at.map(|t| t.elapsed().as_secs())
    }

    /// True only if a spawned child is still alive (reaps it on exit).
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)), // Ok(None) = still alive
            None => false,
        }
    }

    /// Write the generated conf, spawn the dedicated filter-chain host with the
    /// plugin's runtime env, and (optionally) repoint the default input to us.
    ///
    /// MUST be called from the main thread: the spawn below installs
    /// `PR_SET_PDEATHSIG`, which is *thread-scoped* — it binds the child's
    /// lifetime to the spawning thread, not the process. Every caller (the main
    /// event loop's `Cmd`/`Tick` handlers and the `--enable-once` path) runs on
    /// the main thread, so the death-signal fires on process exit as intended.
    pub fn enable(&mut self, cfg: &Config) -> std::io::Result<()> {
        // Whatever asked for this start (watchdog respawn, profile restart,
        // a settings change), a recovery fallback stays one. Only the
        // return, a mic pick or a deliberate turn-on ends it: never a device
        // list, which can be stale right when the mic vanishes.
        let cfg = &restart_config(cfg, self.mic_fallback);
        // A living source host keeps hushmic_source, and with it the default
        // and the takeover: only the chain is replaced, and apps recording
        // HushMic stay on it (silence while the new chain starts, never the
        // raw mic). Without one this is a full start, which ALWAYS tears down
        // first — unconditionally, not just when `is_running()`.
        // The teardown is idempotent (it `.take()`s the child and clears
        // `set_default_active`/`prior_default`), and it is the ONLY thing that
        // restores a previously-captured prior default. If the spawned child has
        // EXITED (crash / fatal conf / broken env), `is_running()` is false, so a
        // guarded `if self.is_running()` would SKIP it: the stale
        // `prior_default` (the user's real device) is never restored, the
        // `default.configured.audio.source` key still points at the dead
        // `hushmic_source`, and the unconditional re-capture below would then read
        // that dead node back as the "prior" default — permanently discarding the
        // user's real default. Restoring first means the re-capture sees the real
        // device again. (This is the watchdog-on-exited-host path.)
        if self.host_running() {
            // "Set as default" turned off: the kept host gives the default
            // back now, not at quit. One try: hushmic_source stays.
            if !cfg.set_default {
                self.hand_back_default(1);
            }
            self.stop_chain();
        } else {
            // One try: a new hushmic_source follows, and a restore that
            // got no answer is kept for it.
            self.stop(1);
        }

        // Robustness: a saved mic that no longer matches a live source (device
        // unplugged, renamed, or re-enumerated) would pin a dead target.object,
        // which links to nothing and leaves the chain silent. Drop it so we
        // follow the system default instead — but only when the probe ran (a
        // failed probe keeps the saved mic; unknown is not gone).
        // Settings follow the ACTIVE device (per-mic profiles): the pinned
        // mic, or the default source when the chain follows it. While the
        // takeover holds the default (a kept host), what the chain follows
        // is the pre-takeover device in `prior_default`.
        let resolve_started = Instant::now();
        let target =
            pipewire::resolve_chain_target(cfg.mic.as_deref(), self.prior_default.as_deref());
        if let (Some(m), None) = (cfg.mic.as_deref(), target.sources.as_ref()) {
            eprintln!(
                "[hushmic] the device list could not be read ({} ms); keeping the saved \
                 microphone '{m}'",
                resolve_started.elapsed().as_millis()
            );
        }
        let effective_mic = target.mic;
        let (follow_pin, profile_device) = follow_target(&FollowFacts {
            prior: self.prior_default.as_deref(),
            in_use: self.in_use.as_deref(),
            takeover: self.set_default_active,
            effective_mic: effective_mic.as_deref(),
            resolved: target.profile_device.as_deref(),
            sources: target.sources.as_deref(),
        });
        let profile = cfg.profile_for(profile_device.as_deref());
        let real = target
            .sources
            .as_deref()
            .map(pipewire::filter_real)
            .unwrap_or_default();
        self.note_mics(&real);
        let profile_name = profile
            .device
            .as_deref()
            .and_then(|d| self.mic_name(cfg, &real, d));
        let (eff_model, eff_attn) = (profile.model.clone(), profile.attn_limit);

        // Preflight the assets. A missing plugin/model/runtime otherwise fails
        // INSIDE the child where `flags = [ nofail ]` hides it completely: the
        // child stays alive, `is_running()` reports healthy, no node appears,
        // and the user gets a generic error icon with no message anywhere.
        // The model checked is the EFFECTIVE one — the profile the conf will
        // actually name.
        let model = self.paths.model_dir.join(format!("{eff_model}.onnx"));
        for (what, p) in [
            ("LADSPA plugin", &self.paths.plugin_so),
            ("model file", &model),
            ("ONNX Runtime library", &self.paths.dylib),
        ] {
            if !p.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "{what} not found at {} — for a source build run \
                         scripts/setup-assets.sh; for a non-/usr install export \
                         the HUSHMIC_*/ORT_DYLIB_PATH variables install.sh printed",
                        p.display()
                    ),
                ));
            }
        }

        if effective_mic.as_deref() != cfg.mic.as_deref() {
            eprintln!(
                "[hushmic] saved microphone '{}' is not present — following the system default",
                cfg.mic.as_deref().unwrap_or_default()
            );
        }
        // PipeWire < 0.3.64 ignores target.object; emit the legacy node.target
        // too so mic selection is honored there (Ubuntu 22.04 ships 0.3.48).
        let legacy = !pipewire::supports_target_object();
        let report_latency = pipewire::supports_latency_report();
        // One adjusted copy carries every effective value (mic + profile).
        let adjusted = Config {
            mic: effective_mic.clone().or(follow_pin),
            model: eff_model,
            attn_limit: eff_attn,
            ..cfg.clone()
        };
        let conf = render_conf(&adjusted, &self.paths, legacy, self.mode, report_latency);
        let path = conf_path();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&path, conf)?;
        // Host first: the chain's output has somewhere to go (the link
        // worker waits for both); no host, no chain.
        if self.host.is_none() {
            self.spawn_host()?;
        }
        let spawned = spawn_chain(
            &self.paths,
            &SpawnSpec {
                chain: crate::diagnostics::Chain::Mic,
                conf: &path,
                model: &model,
                model_id: &adjusted.model,
                inference: adjusted.inference,
                mode: self.mode.control_value() as u32,
                attn_db: adjusted.attn_limit,
                env: voice_hold_env(adjusted.voice_hold),
            },
        )?;
        let generation = spawned.generation;
        self.tee = spawned.tee;
        self.child = Some(spawned.child);
        self.meter = spawned.meter;
        self.control = spawned.control;
        self.chain_generation = Some(generation);
        self.ports_current
            .store(generation, std::sync::atomic::Ordering::Release);
        self.spawned_at = Some(Instant::now());
        let on_done = &mut self.on_link_done;
        self.link
            .get_or_insert_with(|| {
                crate::link::Worker::spawn(on_done.take().unwrap_or_else(|| Box::new(|_| {})))
            })
            .link(generation);
        self.active_mic = effective_mic;
        self.log_profile(&profile, profile_name.as_deref());
        self.rendered = Some(Rendered::of(cfg, profile.device.as_deref()));
        self.active_profile = Some(profile);

        if cfg.set_default && !pipewire::can_set_default() {
            // One line, once: the toggle is hidden in the tray when the
            // sandbox can't do this, but a config written by a native
            // install (or a build that still had the capability) can carry
            // set_default=true into one that doesn't.
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!(
                    "[hushmic] this sandbox cannot change the system default \
                     microphone — select \"HushMic\" directly in your call app or \
                     sound settings"
                );
            });
        }
        // The default is taken once hushmic_source is confirmed, never at a
        // node that never appeared (a broken install must not hijack the
        // mic): the daemon's loop calls ensure_default_takeover() when the
        // node registers (and on every healthy tick), `--enable-once` after
        // its own wait. Not here: that wait would hold the main loop.
        Ok(())
    }

    /// An attempt finished on the takeover worker: apply what it did. The
    /// main loop calls this when told (see [`Self::on_takeover_done`]).
    pub fn takeover_finished(&mut self) {
        if let Some(done) = self.takeover.as_ref().and_then(|w| w.take()) {
            self.apply_takeover(done);
        }
    }

    /// Wait (up to `limit`) for the attempt that is out and apply it:
    /// `--enable-once`, which has no loop to come back to, and tests.
    /// False when the attempt is still out after `limit`: nothing of it
    /// is applied yet.
    pub fn finish_takeover(&mut self, limit: Duration) -> bool {
        let Some(worker) = self.takeover.as_ref() else {
            return true;
        };
        let done = worker.wait(limit);
        let back = !worker.busy();
        if let Some(done) = done {
            self.apply_takeover(done);
        }
        back
    }

    fn apply_takeover(&mut self, done: crate::takeover::Done) {
        use crate::takeover::{Outcome, Write};
        // An attempt for a host that is gone changed nothing: the teardown
        // waited for any write (see disable). Chain restarts keep the host.
        if self.host_generation != Some(done.generation) {
            return;
        }
        let unanswered = done.outcome.unanswered();
        let write = match done.outcome {
            Outcome::NoAnswer => {
                self.takeover_missed("the current default could not be read in time", true);
                return;
            }
            Outcome::Cancelled => return,
            Outcome::Confirmed => {
                self.takeover_landed();
                return;
            }
            Outcome::Picked(picked) => {
                // Not what this takeover found: the user's choice. It never
                // landed, so nothing to tell (one log line).
                self.release_default(&picked);
                return;
            }
            Outcome::Captured { prior, write } => {
                // Recorded (and persisted) before the write: disable()
                // restores or clears whatever the write left behind.
                self.prior_default = prior;
                self.set_default_active = true;
                write
            }
            Outcome::Rewritten(write) => write,
        };
        match write {
            Write::Landed => self.takeover_landed(),
            Write::NoAnswer => {
                self.default_landed = false;
                self.takeover_missed("the write was not confirmed in time", unanswered);
            }
            Write::Dropped => {
                self.default_landed = false;
                self.takeover_missed(
                    "the session manager did not apply the write (insufficient permissions?)",
                    unanswered,
                );
            }
            Write::Unsent => {
                self.default_landed = false;
                self.takeover_missed(
                    "the breadcrumb was not written in time, no write was sent",
                    unanswered,
                );
            }
        }
    }

    /// The takeover's write is confirmed.
    fn takeover_landed(&mut self) {
        if self.takeover_misses > 0 {
            eprintln!("[hushmic] hushmic_source is the default microphone now");
        }
        self.default_landed = true;
        self.takeover_reset();
    }

    /// A takeover attempt that did not complete (`silent`: the session
    /// manager did not answer): logged the first time and when the daemon
    /// backs off; tried again every [`TAKEOVER_BACKOFF`] from then on.
    fn takeover_missed(&mut self, why: &str, silent: bool) {
        self.takeover_misses = self.takeover_misses.saturating_add(1);
        self.takeover_unanswered = silent;
        if self.takeover_misses >= TAKEOVER_TRIES {
            self.takeover_next = Some(Instant::now() + TAKEOVER_BACKOFF);
        }
        if self.takeover_misses == 1 {
            eprintln!("[hushmic] could not make hushmic_source the default microphone: {why}");
        } else if self.takeover_misses == TAKEOVER_TRIES {
            eprintln!(
                "[hushmic] still could not make hushmic_source the default microphone after \
                 {TAKEOVER_TRIES} tries ({why}); trying again every {} s",
                TAKEOVER_BACKOFF.as_secs()
            );
        }
    }

    /// Take the system default for `hushmic_source`: no-op unless
    /// `set_default` is wanted, not taken (or not confirmed) yet, and the
    /// node was confirmed present. Called when the node registers, on every
    /// healthy tick (so a takeover that could not complete is retried) and
    /// by `--enable-once`. Never waits: the attempt runs on the takeover
    /// worker, one at a time, and its outcome comes back through
    /// [`Self::takeover_finished`].
    pub fn ensure_default_takeover(&mut self, cfg: &Config, node_present: bool) {
        self.ensure_default_takeover_at(cfg, node_present, Instant::now());
    }

    /// [`Self::ensure_default_takeover`] as if it were `now` (tests).
    pub fn ensure_default_takeover_at(&mut self, cfg: &Config, node_present: bool, now: Instant) {
        // Not taken yet, or written without landing: try (again), every
        // tick up to TAKEOVER_TRIES in a row, then every TAKEOVER_BACKOFF.
        // Only the user's own pick ends it (release_default).
        let due =
            self.takeover_misses < TAKEOVER_TRIES || self.takeover_next.is_none_or(|t| now >= t);
        let pending = !(self.set_default_active && self.default_landed) && due;
        let Some(generation) = self.host_generation else {
            return;
        };
        if !(cfg.set_default
            && pipewire::can_set_default()
            && pending
            && !self.default_released
            && node_present
            && self.is_running())
        {
            return;
        }
        // Capture exactly once per takeover: when a previous disable() could
        // not restore the prior default (daemon flap), it is still held in
        // prior_default/set_default_active and must not be clobbered by
        // re-reading a key that now points at us.
        let job = if self.set_default_active {
            crate::takeover::Job::Retry {
                prior: self.prior_default.clone(),
            }
        } else {
            crate::takeover::Job::Capture
        };
        let on_done = &mut self.on_takeover_done;
        let caps = self.takeover_caps;
        let hold = self.takeover_hold.clone();
        let worker = self.takeover.get_or_insert_with(|| {
            crate::takeover::Worker::spawn(
                on_done.take().unwrap_or_else(|| Box::new(|| {})),
                caps,
                hold,
            )
        });
        if !worker.busy() {
            worker.start(generation, job);
        }
    }

    /// Restore the prior default (before killing our node so clients don't
    /// briefly land on a dead source) and tear down the child: Off and
    /// Quit. Nothing of ours follows, so the restore is patient
    /// ([`RESTORE_TRIES`]).
    pub fn disable(&mut self) -> std::io::Result<()> {
        self.stop(RESTORE_TRIES);
        Ok(())
    }

    /// Hand the default back (`tries` as in [`Self::hand_back_default`]),
    /// then stop the chain and the host.
    fn stop(&mut self, tries: u32) {
        self.hand_back_default(tries);
        self.stop_chain();
        self.stop_host();
    }

    /// Give the default back if this run took it: the prior default, or no
    /// configured default at all. Done while hushmic_source still exists,
    /// so clients never briefly land on a dead source. A restore PipeWire
    /// does not answer is tried `tries` times in all.
    fn hand_back_default(&mut self, tries: u32) {
        // A takeover write being sent goes out before the restore below
        // (which then lands after it), and what it did is applied first:
        // it is one call, waited for as long as its cap (the read-back is
        // not waited for). An attempt still reading, or still writing its
        // breadcrumb, is told not to write at all.
        let wait = self.takeover_caps.call + TAKEOVER_WRITE_SLACK;
        if let Some(done) = self.takeover.as_ref().and_then(|w| w.cancel(wait)) {
            self.apply_takeover(done);
        }
        // Only undo the default if *this* run set it. Restore the prior default
        // when there was one; otherwise delete the key so it doesn't dangle on
        // the soon-to-be-dead `hushmic_source`. Done BEFORE killing the child so
        // clients never briefly land on a dead source.
        if self.set_default_active {
            // Only forget the prior default once it is actually restored: this
            // runs precisely when PipeWire is most likely flapping (watchdog
            // re-enable after a daemon restart), and dropping the one copy of
            // the user's device on a failed pw-metadata call would lose it for
            // good. On failure everything (incl. the on-disk copy) is kept for
            // the next disable/Drop/startup-recovery to retry.
            let prior = self.prior_default.clone();
            let restored = restore_patiently(
                tries,
                || match prior.as_deref() {
                    Some(prev) => pipewire::set_default_source(prev),
                    None => pipewire::clear_default_source(),
                },
                |n| {
                    eprintln!(
                        "[hushmic] restoring the previous default microphone: PipeWire did \
                         not answer (try {n} of {tries}); trying again"
                    );
                },
            );
            match restored {
                Ok(()) => {
                    // the line a quit leaves in the log
                    match prior.as_deref() {
                        Some(prev) => eprintln!("[hushmic] restored default source: {prev}"),
                        None => {
                            eprintln!("[hushmic] cleared the default source (none was set before)")
                        }
                    }
                    self.prior_default = None;
                    self.set_default_active = false;
                    self.default_landed = false;
                    self.takeover_reset();
                    clear_persisted_prior_default();
                }
                Err((e, n)) => {
                    let why = if e.kind() != std::io::ErrorKind::TimedOut {
                        e.to_string()
                    } else if tries > 1 {
                        format!("PipeWire did not answer (try {n} of {tries})")
                    } else {
                        "PipeWire did not answer".to_string()
                    };
                    eprintln!(
                        "[hushmic] could not restore the previous default microphone: {why}. \
                         It stays recorded and is put back the next time HushMic starts or stops"
                    );
                }
            }
        }
    }

    /// Stop and reap the chain, SIGTERM first (the host and the default stay).
    fn stop_chain(&mut self) {
        if let Some(l) = &self.link {
            l.forget();
        }
        if let Some(mut c) = self.child.take() {
            stop_process(&mut c, "microphone chain");
        }
        if let Some(m) = self.meter.take() {
            crate::meter::remove(&m);
        }
        if let Some(c) = self.control.take() {
            c.writer.unlink_if_at(&c.path);
        }
        self.chain_generation = None;
        self.ports_current
            .store(0, std::sync::atomic::Ordering::Release);
        // Forget the tier and retire this chain's generation FIRST: a tee
        // that outlives the wait below is then writing under a generation
        // nobody reads, so the wait is only about draining the log, never
        // about correctness.
        crate::diagnostics::new_engine_generation();
        // The pipe closes with the child; give the tee a moment to drain
        // its last lines into the log.
        if let Some(t) = self.tee.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
        self.spawned_at = None;
        self.active_mic = None;
        self.active_profile = None;
        self.rendered = None;
    }

    /// Stop and reap the source host, SIGTERM first (after the default was
    /// handed back).
    fn stop_host(&mut self) {
        if let Some(mut h) = self.host.take() {
            stop_process(&mut h, "source host");
        }
        self.host_spawned_at = None;
        self.host_generation = None;
    }

    /// Spawn the source host. Main thread only, like the chain
    /// (`PR_SET_PDEATHSIG` is thread-scoped; see [`Self::enable`]).
    fn spawn_host(&mut self) -> std::io::Result<()> {
        let path = host_conf_path();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&path, render_host_conf())?;
        let mut command = Command::new("pipewire");
        command.arg("-c").arg(&path);
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        self.host = Some(command.spawn()?);
        self.host_spawned_at = Some(Instant::now());
        self.host_seq += 1;
        self.host_generation = Some(self.host_seq);
        // A new host gets the takeover's full count of tries again.
        self.takeover_reset();
        Ok(())
    }

    /// The running source host's generation (None without one): a new
    /// value is a new `hushmic_source`.
    pub fn host_generation(&self) -> Option<u64> {
        self.host_generation
    }

    /// The source host is alive (reaps it on exit).
    pub fn host_running(&mut self) -> bool {
        match self.host.as_mut() {
            Some(h) => matches!(h.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Seconds since the source host was spawned (None = none).
    pub fn secs_since_host_spawn(&self) -> Option<u64> {
        self.host_spawned_at.map(|t| t.elapsed().as_secs())
    }

    /// Stop everything, the host too, and start again: its nodes went with
    /// a PipeWire restart while its process lives on, or it died. Apps on
    /// HushMic move to the restored default meanwhile, as at a first start.
    pub fn restart_all(&mut self, cfg: &Config) -> std::io::Result<()> {
        self.stop(1);
        self.enable(cfg)
    }

    /// Link the running chain into the host again (the link went missing
    /// while both are up). Nothing while a link job is out.
    pub fn relink(&self) {
        if let (Some(g), Some(l)) = (self.chain_generation, self.link.as_ref()) {
            if !l.busy() {
                l.link(g);
            }
        }
    }

    /// Remove links that must not be on the chain's output or the host's
    /// feed ([`pipewire::ChainGraph::foreign`]).
    pub fn unlink(&self, ids: Vec<u32>) {
        if let Some(l) = &self.link {
            l.unlink(ids);
        }
    }

    /// Cut the links of streams linked into the capture or the feed
    /// ([`pipewire::IntruderGuard::to_cut`]).
    pub fn cut(&self, links: Vec<(u32, String, String)>) {
        if let Some(l) = &self.link {
            l.cut(links);
        }
    }

    /// Wait (up to `limit`) until the running chain is linked into the
    /// host.
    pub fn wait_linked(&self, limit: Duration) -> bool {
        self.wait_link(limit) == Some(crate::link::Outcome::Linked)
    }

    /// Wait (up to `limit`) for how the running chain's link job ended;
    /// None while it is still out (or no chain runs).
    pub fn wait_link(&self, limit: Duration) -> Option<crate::link::Outcome> {
        match (self.chain_generation, self.link.as_ref()) {
            (Some(g), Some(l)) => l.wait(g, limit),
            _ => None,
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.disable(); // clean teardown + default restore on quit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restore_is_tried_again_only_while_there_is_no_answer() {
        use std::io::{Error, ErrorKind};
        let silent = || Error::new(ErrorKind::TimedOut, "no answer");
        let run = |tries: u32, script: Vec<std::io::Result<()>>| {
            let mut script = script.into_iter();
            let (mut calls, mut misses) = (0, Vec::new());
            let ended = restore_patiently(
                tries,
                || {
                    calls += 1;
                    script.next().expect("one try too many")
                },
                |n| misses.push(n),
            );
            (ended.map_err(|(e, n)| (e.kind(), n)), calls, misses)
        };
        // no answer, twice, then it lands: each miss is told with its number
        assert_eq!(
            run(3, vec![Err(silent()), Err(silent()), Ok(())]),
            (Ok(()), 3, vec![1, 2])
        );
        // never an answer: the last try's error, and no fourth
        assert_eq!(
            run(3, vec![Err(silent()), Err(silent()), Err(silent())]),
            (Err((ErrorKind::TimedOut, 3)), 3, vec![1, 2])
        );
        // an answer is final, the first one included
        assert_eq!(
            run(3, vec![Err(Error::other("not applied"))]),
            (Err((ErrorKind::Other, 1)), 1, vec![])
        );
        assert_eq!(
            run(3, vec![Err(silent()), Err(Error::other("not applied"))]),
            (Err((ErrorKind::Other, 2)), 2, vec![1])
        );
        assert_eq!(run(3, vec![Ok(())]), (Ok(()), 1, vec![]));
        // one try: no answer ends it
        assert_eq!(
            run(1, vec![Err(silent())]),
            (Err((ErrorKind::TimedOut, 1)), 1, vec![])
        );
    }

    #[test]
    fn terminate_asks_first_then_kills_and_always_reaps() {
        // exits on SIGTERM: quickly, without the kill
        let mut c = Command::new("sleep").arg("30").spawn().unwrap();
        let ended = terminate(&mut c, Duration::from_secs(2));
        assert!(
            matches!(ended, Ended::Termed(d) if d < Duration::from_millis(500)),
            "{ended:?}"
        );
        assert!(c.try_wait().unwrap().is_some());
        // ignores SIGTERM: killed once the grace is over, and reaped
        let mut c = Command::new("sh")
            .args(["-c", "trap '' TERM; echo ready; sleep 30"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(c.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        let started = Instant::now();
        assert_eq!(terminate(&mut c, Duration::from_millis(150)), Ended::Killed);
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(150) && took < Duration::from_secs(2),
            "{took:?}"
        );
        assert!(c.try_wait().unwrap().is_some());
        // gone already
        let mut c = Command::new("true").spawn().unwrap();
        let _ = c.wait();
        assert_eq!(terminate(&mut c, TERM_GRACE), Ended::Gone);
    }
    use crate::config::Inference;

    #[test]
    fn an_empty_prior_default_never_replaces_a_known_mic() {
        let root = std::env::temp_dir().join(format!("hushmic-prior-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let p = root.join("prior-default");
        let read = || std::fs::read_to_string(&p).unwrap();
        // nothing known yet: "none before" is recorded
        persist_prior_default_in(&p, "");
        assert_eq!(read(), "");
        persist_prior_default_in(&p, "usb_mic");
        assert_eq!(read(), "usb_mic");
        // found nothing better: the known mic stays
        persist_prior_default_in(&p, "");
        assert_eq!(read(), "usb_mic");
        // another real mic replaces it
        persist_prior_default_in(&p, "laptop_mic");
        assert_eq!(read(), "laptop_mic");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generated_files_move_out_of_the_config_folder() {
        let root = std::env::temp_dir().join(format!("hushmic-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new) = (root.join("config/hushmic"), root.join("state/hushmic"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.toml"), "attn_limit = 24.0\n").unwrap();
        std::fs::write(old.join("filter-chain.conf"), "old chain").unwrap();
        std::fs::write(old.join("source.conf"), "old host").unwrap();
        std::fs::write(old.join("prior-default"), "usb_mic").unwrap();
        migrate_state_in(&old, &new);
        assert_eq!(
            std::fs::read_to_string(new.join("prior-default")).unwrap(),
            "usb_mic"
        );
        assert_eq!(
            std::fs::read_to_string(new.join("filter-chain.conf")).unwrap(),
            "old chain"
        );
        assert!(new.join("source.conf").exists());
        for f in STATE_FILES {
            assert!(!old.join(f).exists(), "{f} left in the config folder");
        }
        assert!(old.join("config.toml").exists(), "the config stays");
        // a newer copy wins; the old one goes
        std::fs::write(old.join("prior-default"), "stale").unwrap();
        migrate_state_in(&old, &new);
        assert_eq!(
            std::fs::read_to_string(new.join("prior-default")).unwrap(),
            "usb_mic"
        );
        assert!(!old.join("prior-default").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    // --- ChainDelta: the live-vs-restart decision ---

    const HQ: &str = "dpdfnet8_48khz_hr";

    fn rendered(cfg: &Config, device: Option<&str>) -> Rendered {
        Rendered::of(cfg, device)
    }

    #[test]
    fn a_strength_change_alone_is_a_live_write() {
        let a = Config::default();
        let b = Config {
            attn_limit: 24.0,
            ..a.clone()
        };
        let running = rendered(&a, None);
        assert_eq!(
            ChainDelta::of(Some(&running), &rendered(&b, None)),
            ChainDelta::Attn(24.0)
        );
        assert_eq!(
            ChainDelta::of(Some(&running), &rendered(&a, None)),
            ChainDelta::Unchanged
        );
        assert!(!ChainDelta::Attn(24.0).restarts());
    }

    #[test]
    fn what_the_conf_or_spawn_carries_restarts() {
        let a = Config::default();
        let running = rendered(&a, None);
        for b in [
            Config {
                model: LIGHT_MODEL.into(),
                ..a.clone()
            },
            Config {
                mic: Some("alsa_input.rode".into()),
                ..a.clone()
            },
            Config {
                set_default: true,
                ..a.clone()
            },
            Config {
                inference: crate::config::Inference::Onnx,
                ..a.clone()
            },
            // the plugin reads the voice hold when it is instantiated
            Config {
                voice_hold: true,
                ..a.clone()
            },
        ] {
            assert_eq!(
                ChainDelta::of(Some(&running), &rendered(&b, None)),
                ChainDelta::Restart,
                "{b:?}"
            );
        }
        // No live child: always a (re)start.
        assert_eq!(ChainDelta::of(None, &running), ChainDelta::Restart);
    }

    #[test]
    fn the_voice_hold_reaches_the_plugin_as_its_variable_only_when_on() {
        assert_eq!(voice_hold_env(true), [(VOICE_HOLD_ENV, "1")]);
        assert!(voice_hold_env(false).is_empty());
        assert_eq!(VOICE_HOLD_ENV, "HUSHMIC_VOICE_HOLD");
    }

    #[test]
    fn a_profile_strength_edit_is_live_and_a_global_one_leaves_it_alone() {
        let mut a = Config {
            mic: Some("rode".into()),
            ..Config::default()
        };
        a.set_attn_for(Some("rode"), 12.0);
        let running = rendered(&a, Some("rode"));
        assert_eq!(running.attn, 12.0);
        // The profile's own strength moves: live.
        let mut b = a.clone();
        b.set_attn_for(Some("rode"), 6.0);
        assert_eq!(
            ChainDelta::of(Some(&running), &rendered(&b, Some("rode"))),
            ChainDelta::Attn(6.0)
        );
        // Only the defaults move: the chain on the profile is unchanged.
        let c = Config {
            attn_limit: 24.0,
            ..a.clone()
        };
        assert_eq!(
            ChainDelta::of(Some(&running), &rendered(&c, Some("rode"))),
            ChainDelta::Unchanged
        );
        // Same model in a new profile, new strength: still live.
        let mut d = Config::default();
        d.mic_prefs.insert(
            "webcam".into(),
            crate::config::MicPrefs {
                model: HQ.into(),
                attn_limit: 24.0,
            },
        );
        assert_eq!(
            ChainDelta::of(
                Some(&rendered(&Config::default(), Some("webcam"))),
                &rendered(&d, Some("webcam"))
            ),
            ChainDelta::Attn(24.0)
        );
    }

    fn src(name: &str) -> pipewire::Source {
        pipewire::Source {
            name: name.into(),
            description: name.into(),
        }
    }

    /// UX panel A2: following a default that is gone, a start keeps the
    /// device the chain fell back to, and runs with its settings.
    #[test]
    fn a_start_after_the_followed_default_left_keeps_the_fallback() {
        let both = [src("jabra"), src("laptop")];
        let laptop = [src("laptop")];
        let facts = |prior, in_use, sources| FollowFacts {
            prior,
            in_use,
            takeover: true,
            effective_mic: None,
            resolved: prior.filter(|_| sources == &both[..]),
            sources: Some(sources),
        };
        // the default it follows is there: pinned to it, its settings
        assert_eq!(
            follow_target(&facts(Some("jabra"), Some("jabra"), &both[..])),
            (Some("jabra".into()), Some("jabra".into()))
        );
        // ... and still there after a fallback was seen: it wins
        assert_eq!(
            follow_target(&facts(Some("jabra"), Some("laptop"), &both[..])),
            (Some("jabra".into()), Some("jabra".into()))
        );
        // gone: the device the chain fell back to
        assert_eq!(
            follow_target(&facts(Some("jabra"), Some("laptop"), &laptop[..])),
            (Some("laptop".into()), Some("laptop".into()))
        );
        // gone, and the fallback unknown or gone too: nothing to pin to
        assert_eq!(
            follow_target(&facts(Some("jabra"), None, &laptop[..])),
            (None, None)
        );
        assert_eq!(
            follow_target(&facts(Some("jabra"), Some("webcam"), &laptop[..])),
            (None, None)
        );
        // pinned to a mic: neither applies
        let pinned = FollowFacts {
            effective_mic: Some("laptop"),
            resolved: Some("laptop"),
            ..facts(Some("jabra"), Some("laptop"), &laptop[..])
        };
        assert_eq!(follow_target(&pinned), (None, Some("laptop".into())));
        // no takeover: the default is the device's own, nothing pinned
        let own = FollowFacts {
            takeover: false,
            ..facts(Some("jabra"), Some("laptop"), &laptop[..])
        };
        assert_eq!(follow_target(&own), (None, Some("laptop".into())));
    }

    /// UX panel A2: an edit while the chain follows the default lands on
    /// the device the chain captures from, not the one it started on.
    #[test]
    fn an_edit_goes_to_the_microphone_in_use() {
        let mut c = Controller::new(Paths {
            plugin_so: PathBuf::from("/nonexistent/plugin.so"),
            model_dir: PathBuf::from("/nonexistent/models"),
            dylib: PathBuf::from("/nonexistent/libonnxruntime.so"),
        });
        let mut cfg = Config::default();
        cfg.set_attn_for(Some("jabra"), 100.0);
        cfg.set_attn_for(Some("laptop"), 24.0);
        // no chain: nothing is in use
        c.observe_in_use(Some("laptop".into()));
        assert_eq!(c.in_use(), None);
        assert!(!c.settle_in_use(&cfg));
        // a chain that follows the default, started on the headset
        c.active_profile = Some(cfg.profile_for(Some("jabra")));
        c.observe_in_use(Some("jabra".into()));
        assert!(!c.settle_in_use(&cfg), "on the device it started on");
        // the headset left: the chain captures from the laptop
        c.observe_in_use(Some("laptop".into()));
        assert_eq!(c.in_use(), Some("laptop"));
        assert!(c.settle_in_use(&cfg));
        let p = c.active_profile().unwrap();
        assert_eq!(p.device.as_deref(), Some("laptop"));
        assert_eq!(p.attn_limit, 24.0);
        assert!(!c.settle_in_use(&cfg), "once");
        // pinned to a mic: the pin decides, not the graph
        c.active_mic = Some("jabra".into());
        c.observe_in_use(Some("laptop".into()));
        assert_eq!(c.in_use(), None);
        assert!(!c.settle_in_use(&cfg));
    }

    #[test]
    fn only_onnx_is_passed_on_and_an_override_wins() {
        assert_eq!(plugin_inference(Inference::Onnx, false), Some("onnx"));
        assert_eq!(plugin_inference(Inference::Auto, false), None);
        assert_eq!(plugin_inference(Inference::Onnx, true), None);
        assert_eq!(plugin_inference(Inference::Auto, true), None);
    }

    #[test]
    fn logs_and_status_name_twins_as_the_window_does() {
        crate::i18n::pin_english();
        let mut c = Controller::new(Paths {
            plugin_so: PathBuf::from("/nonexistent/plugin.so"),
            model_dir: PathBuf::from("/nonexistent/models"),
            dylib: PathBuf::from("/nonexistent/libonnxruntime.so"),
        });
        let cfg = Config::default();
        let twin = |name: &str, description: &str| pipewire::Source {
            name: name.into(),
            description: description.into(),
        };
        let mics = [
            twin("usb-rode-a", "RODE NT-USB"),
            twin("usb-rode-b", "RODE NT-USB"),
        ];
        c.note_mics(&mics);
        assert_eq!(
            c.mic_name(&cfg, &mics, "usb-rode-b").as_deref(),
            Some("RODE NT-USB (2)")
        );
        // The first twin unplugged: the other keeps its number, and the
        // unplugged one its name.
        assert_eq!(
            c.mic_name(&cfg, &mics[1..], "usb-rode-b").as_deref(),
            Some("RODE NT-USB (2)")
        );
        assert_eq!(
            c.mic_name(&cfg, &[], "usb-rode-a").as_deref(),
            Some("RODE NT-USB")
        );
        // A device keeps the first description seen this run.
        c.note_mics(&[twin("usb-rode-a", "Podcast")]);
        assert_eq!(c.seen_mics()["usb-rode-a"], "RODE NT-USB");
        assert_eq!(c.mic_name(&cfg, &mics, "usb-other"), None);
    }
}
