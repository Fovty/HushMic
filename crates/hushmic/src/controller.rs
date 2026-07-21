use crate::config::Config;
use crate::pipewire;
use directories::ProjectDirs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Filesystem locations the controller needs to spawn the filter-chain host.
///
/// `plugin_so` is the LADSPA `.so`, `model_dir` holds the `<model>.onnx`
/// files, and `dylib` is the ONNX Runtime shared object that the plugin
/// `dlopen`s via the `ORT_DYLIB_PATH` env var.
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

/// The cpu ids in this process's CURRENT affinity mask — cgroup cpusets and
/// any deliberate taskset/CPUAffinity= restriction included. None if
/// unreadable. Sorted ascending by construction.
fn current_affinity() -> Option<Vec<usize>> {
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

fn conf_path() -> PathBuf {
    ProjectDirs::from("io", "hushmic", "hushmic")
        .expect("home")
        .config_dir()
        .join("filter-chain.conf")
}

/// Where the pre-takeover system default is persisted while `set_default` is
/// active, so a run that dies without restoring it (SIGKILL, power loss) can
/// be repaired by the next launch — see [`recover_dangling_default`].
fn prior_default_path() -> PathBuf {
    ProjectDirs::from("io", "hushmic", "hushmic")
        .expect("home")
        .config_dir()
        .join("prior-default")
}

fn persist_prior_default(name: &str) {
    // Atomic: this breadcrumb exists precisely for crash recovery, so it
    // must not itself be truncatable by the crash (the empty-file case is
    // tolerated by the reader, but then the real prior default is lost).
    let _ = crate::fsutil::atomic_write(&prior_default_path(), name.as_bytes());
}

/// A previous run's persisted prior default, if any (crash leftover). Never
/// yields our own node name.
fn read_persisted_prior_default() -> Option<String> {
    let s = std::fs::read_to_string(prior_default_path()).ok()?;
    let s = s.trim();
    if s.is_empty() || s == "hushmic_source" {
        None
    } else {
        Some(s.to_string())
    }
}

fn clear_persisted_prior_default() {
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
fn spa_escape(s: &str) -> String {
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
/// The filter-chain is mono, 48 kHz, and (when a mic is chosen) pins the mic
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
context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}}
context.modules = [
  {{ name = libpipewire-module-rt
    args = {{ }}
    flags = [ ifexists nofail ]
  }}
  {{ name = libpipewire-module-protocol-native }}
  {{ name = libpipewire-module-client-node }}
  {{ name = libpipewire-module-adapter }}
  {{ name = libpipewire-module-metadata }}
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
        node.name      = "hushmic_input"
        node.passive   = true
        audio.rate     = 48000
        audio.channels = 1
        audio.position = [ MONO ]
{target}      }}
      playback.props = {{
        node.name        = "hushmic_source"
        node.description  = "HushMic"
        media.class      = Audio/Source
        audio.rate       = 48000
        audio.channels   = 1
        audio.position   = [ MONO ]
      }}
    }}
  }}
]
"#,
        plugin = spa_escape(&paths.plugin_so.display().to_string()),
        attn = attn,
        mode = mode.control_value(),
        target = target,
        latency_node = latency_node,
        links = links,
    )
}

/// The chain's algorithmic latency in samples at 48 kHz: 480 (STFT
/// framing) + 1920 (the model's 4-hop group delay) + 480 (the plugin's
/// one-hop output prefill) = 2880 = 60 ms. MEASURED, not derived — the
/// dpdfnet-ladspa `latency_probe` tests push impulses and real speech
/// through the actual DSP and pin the engine part at exactly 2400 for
/// both models; change the DSP and those tests force this constant to be
/// re-derived. PipeWire adds its own quantum/device buffering on top.
pub const LATENCY_SAMPLES: u32 = 2880;

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

/// Owns the `pipewire -c` child that hosts the virtual mic, plus the prior
/// system default source so it can be restored on teardown.
pub struct Controller {
    paths: Paths,
    child: Option<Child>,
    prior_default: Option<String>,
    /// True when this run repointed the system default to `hushmic_source`, so
    /// `disable` knows it must restore the prior default (if any) or otherwise
    /// clear the now-dangling key. Distinguishes "we set it" from "prior_default
    /// happened to be None", which a bare `Option` could not. Stays true when a
    /// restore attempt FAILS (daemon flap), so the prior default is retried
    /// later instead of being dropped.
    set_default_active: bool,
    spawned_at: Option<Instant>,
    /// The mic the running child's conf pins (None = follows the system
    /// default). Set only once a child actually spawned; cleared by
    /// `disable()` — see [`Controller::active_mic`].
    active_mic: Option<String>,
    /// The processing mode every spawn renders into the conf. Living here
    /// (not per-`enable` argument) is what makes the mode survive automatic
    /// restarts — mic recovery re-enabling the chain must never silently
    /// unmute the mic.
    mode: RunMode,
}

impl Controller {
    pub fn new(paths: Paths) -> Self {
        Controller {
            paths,
            child: None,
            prior_default: None,
            set_default_active: false,
            spawned_at: None,
            active_mic: None,
            mode: RunMode::default(),
        }
    }

    pub fn mode(&self) -> RunMode {
        self.mode
    }

    /// Record the mode future spawns render into the conf. Does NOT touch a
    /// running child — the live set-param path (or a restart) does that.
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
        // ALWAYS tear down first — unconditionally, not just when `is_running()`.
        // `disable()` is idempotent (it `.take()`s the child and clears
        // `set_default_active`/`prior_default`), and it is the ONLY thing that
        // restores a previously-captured prior default. If the spawned child has
        // EXITED (crash / fatal conf / broken env), `is_running()` is false, so a
        // guarded `if self.is_running()` would SKIP `disable()`: the stale
        // `prior_default` (the user's real device) is never restored, the
        // `default.configured.audio.source` key still points at the dead
        // `hushmic_source`, and the unconditional re-capture below would then read
        // that dead node back as the "prior" default — permanently discarding the
        // user's real default. Restoring first means the re-capture sees the real
        // device again. (This is the watchdog-on-exited-child path.)
        self.disable()?;

        // Robustness: a saved mic that no longer matches a live source (device
        // unplugged, renamed, or re-enumerated) would pin a dead target.object,
        // which links to nothing and leaves the chain silent. Drop it so we
        // follow the system default instead — but only when the probe ran (a
        // failed probe keeps the saved mic; unknown is not gone).
        let effective_mic = pipewire::resolve_effective_mic(
            cfg.mic.as_deref(),
            pipewire::sources_snapshot().as_deref(),
        );
        // Settings follow the ACTIVE device (per-mic profiles): the default
        // source is only probed when the chain will follow it — the one case
        // where its profile applies.
        let default_source = if effective_mic.is_none() {
            pipewire::get_default_source()
        } else {
            None
        };
        let (eff_model, eff_attn) =
            cfg.effective_settings(effective_mic.as_deref(), default_source.as_deref());

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
            mic: effective_mic.clone(),
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
        // Dedicated filter-chain host; env propagates to the plugin's dlopen.
        let mut command = Command::new("pipewire");
        command
            .arg("-c")
            .arg(&path)
            .env("HUSHMIC_MODEL_PATH", &model)
            .env("ORT_DYLIB_PATH", &self.paths.dylib);
        // Bind the host's lifetime to ours: if hushmic dies ungracefully
        // (crash, SIGKILL, session logout) Drop never runs, so without this the
        // child would linger and keep advertising a dead `hushmic_source` as the
        // default mic. PR_SET_PDEATHSIG makes the kernel SIGTERM the child when
        // the spawning (main) thread exits, guaranteeing teardown.
        //
        // INVARIANT: PR_SET_PDEATHSIG is THREAD-scoped — it ties the child to the
        // thread that calls prctl, not to the process. This is correct ONLY
        // because `enable()` is always called from the main thread (see the
        // `enable()` doc comment); were it called from a transient worker thread,
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
                    let _ =
                        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
                }
                Ok(())
            });
        }
        // Pipe the child's stderr through the diagnostics tee: every line
        // still reaches our own stderr, and the persisted copy is what
        // `--doctor` / "Copy diagnostics" include (also post-mortem).
        command.stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        if let Some(stderr) = child.stderr.take() {
            crate::diagnostics::spawn_stderr_tee(stderr, crate::diagnostics::log_path());
        }
        self.child = Some(child);
        self.spawned_at = Some(Instant::now());
        self.active_mic = effective_mic;

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
        if cfg.set_default && pipewire::can_set_default() {
            // Never repoint the system default at a node that never appeared:
            // wait (bounded) for hushmic_source to register first. On timeout
            // the user's default is left completely untouched — a broken
            // install must not hijack the mic, and the watchdog's next attempt
            // lands here again anyway.
            if pipewire::wait_for_hushmic_source(Duration::from_secs(2)) {
                self.take_default();
            } else {
                // NOT permanent: the watchdog completes the takeover via
                // ensure_default_takeover() once the node shows up late.
                eprintln!(
                    "[hushmic] hushmic_source did not appear yet; leaving the system \
                     default microphone untouched for now"
                );
            }
        }
        Ok(())
    }

    /// Repoint the system default at `hushmic_source`, capturing the prior
    /// default exactly once per takeover. Callers must have confirmed the node
    /// is present — never repoint the default at a node that isn't there.
    fn take_default(&mut self) {
        // Capture exactly once per takeover: when a previous disable() could
        // not restore the prior default (daemon flap), it is still held in
        // prior_default/set_default_active and must not be clobbered by
        // re-reading a key that now points at us.
        if !self.set_default_active {
            // May be None on a machine with no configured default; we still
            // record that *we* set it so `disable` clears the key rather than
            // strand it on the dead node. Never record our OWN node as the
            // prior default — and when the live key IS our node (or
            // unreadable), a crashed run's persisted copy is strictly more
            // informative than the fresh capture: startup recovery keeps its
            // file when the daemon wasn't up yet (login autostart races
            // WirePlumber), and clobbering it with "" would lose the user's
            // device for good.
            self.prior_default = match pipewire::get_default_source() {
                Some(name) if name == "hushmic_source" => read_persisted_prior_default(),
                None => read_persisted_prior_default(),
                other => other,
            };
            // Survive a crash while the takeover is active.
            persist_prior_default(self.prior_default.as_deref().unwrap_or(""));
            self.set_default_active = true;
        }
        // The read-back inside set_default_source is what detects silently
        // dropped writes (restricted sandboxes) — a swallowed Err here
        // would defeat it. State is deliberately NOT rolled back: the
        // breadcrumb and set_default_active make disable() restore/clear
        // whatever half-state the failed attempt left behind.
        if let Err(e) = pipewire::set_default_source("hushmic_source") {
            eprintln!("[hushmic] could not make hushmic_source the default microphone: {e}");
        }
    }

    /// Complete a pending default takeover: no-op unless `set_default` is
    /// wanted, not yet active, and the node was confirmed present. Called by
    /// the watchdog so a node that registered AFTER enable()'s bounded wait
    /// still gets the takeover — otherwise it would silently never happen.
    pub fn ensure_default_takeover(&mut self, cfg: &Config, node_present: bool) {
        if cfg.set_default
            && pipewire::can_set_default()
            && !self.set_default_active
            && node_present
            && self.is_running()
        {
            self.take_default();
        }
    }

    /// Restore the prior default (before killing our node so clients don't
    /// briefly land on a dead source) and tear down the child.
    pub fn disable(&mut self) -> std::io::Result<()> {
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
            let restored = match self.prior_default.as_deref() {
                Some(prev) => pipewire::set_default_source(prev).is_ok(),
                None => pipewire::clear_default_source().is_ok(),
            };
            if restored {
                self.prior_default = None;
                self.set_default_active = false;
                clear_persisted_prior_default();
            } else {
                eprintln!(
                    "[hushmic] could not restore the previous default microphone \
                     (PipeWire unreachable?); will retry"
                );
            }
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.spawned_at = None;
        self.active_mic = None;
        Ok(())
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.disable(); // clean teardown + default restore on quit
    }
}
