use crate::config::Config;
use crate::pipewire;
use directories::ProjectDirs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Filesystem locations the controller needs to spawn the filter-chain host.
///
/// `plugin_so` is the v0.1 LADSPA `.so`, `model_dir` holds the `<model>.onnx`
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
    let p = prior_default_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&p, name);
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
/// v0.1 Task 7 proved that a bare filter-chain *fragment*
/// (`context.modules = [ filter-chain ]`) fails to load standalone with
/// `can't find protocol 'PipeWire:Protocol:Native'` because it carries none of
/// the core modules. Since the controller spawns exactly `pipewire -c <conf>`,
/// this emits the base preamble (`context.properties`, `context.spa-libs`, and
/// the base `context.modules`: rt, protocol-native, client-node, adapter,
/// metadata — mirroring `/usr/share/pipewire/filter-chain.conf` and
/// `minimal.conf`) and then appends the hushmic filter-chain module.
///
/// The filter-chain is mono, 48 kHz, and (when a mic is chosen) pins
/// `target.object`. Pure function — no I/O.
pub fn render_conf(cfg: &Config, paths: &Paths) -> String {
    // target.object line only when a specific mic is chosen; otherwise the
    // filter-chain follows the system default capture device.
    let target = match &cfg.mic {
        Some(name) => format!("        target.object  = \"{}\"\n", spa_escape(name)),
        None => String::new(),
    };
    // Belt-and-suspenders (Config::load already sanitizes): a non-finite value
    // would render as a literal `NaN`/`inf` token that PipeWire rejects.
    let attn = if cfg.attn_limit.is_finite() {
        cfg.attn_limit.clamp(0.0, 100.0)
    } else {
        100.0
    };
    format!(
        r#"# hushmic self-contained PipeWire filter-chain host (generated; do not edit).
# Base modules mirror /usr/share/pipewire/filter-chain.conf so a bare
# `pipewire -c <this>` has the core protocol + node infrastructure; the hushmic
# filter-chain module is then appended. See run-filter-chain.md (v0.1 Task 7).
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
            control = {{ "Attenuation Limit (dB)" = {attn} }}
          }}
        ]
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
        target = target,
    )
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
}

impl Controller {
    pub fn new(paths: Paths) -> Self {
        Controller {
            paths,
            child: None,
            prior_default: None,
            set_default_active: false,
            spawned_at: None,
        }
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

        // Preflight the assets. A missing plugin/model/runtime otherwise fails
        // INSIDE the child where `flags = [ nofail ]` hides it completely: the
        // child stays alive, `is_running()` reports healthy, no node appears,
        // and the user gets a generic error icon with no message anywhere.
        let model = self.paths.model_dir.join(format!("{}.onnx", cfg.model));
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

        let conf = render_conf(cfg, &self.paths);
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
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        self.child = Some(child);
        self.spawned_at = Some(Instant::now());

        if cfg.set_default {
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
        let _ = pipewire::set_default_source("hushmic_source");
    }

    /// Complete a pending default takeover: no-op unless `set_default` is
    /// wanted, not yet active, and the node was confirmed present. Called by
    /// the watchdog so a node that registered AFTER enable()'s bounded wait
    /// still gets the takeover — otherwise it would silently never happen.
    pub fn ensure_default_takeover(&mut self, cfg: &Config, node_present: bool) {
        if cfg.set_default && !self.set_default_active && node_present && self.is_running() {
            self.take_default();
        }
    }

    /// Restore the prior default (before killing our node so clients don't
    /// briefly land on a dead source) and tear down the child.
    pub fn disable(&mut self) -> std::io::Result<()> {
        // Only undo the default if *this* run set it. Restore the prior default
        // when there was one; otherwise delete the key so it doesn't dangle on
        // the soon-to-be-dead `hushmic_source`. Done BEFORE killing the child so
        // clients never briefly land on a dead source. `.take()` keeps this
        // idempotent across the explicit disable + Drop path.
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
        Ok(())
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.disable(); // clean teardown + default restore on quit
    }
}
