use hushmic::config::Config;
use hushmic::control;
use hushmic::controller::{self, Controller, Paths, RunMode};
use hushmic::notify::{self, FailureGate, Slot};
use hushmic::pipewire;
use hushmic::tr;
use hushmic::tray::{HushMicTray, TrayCmd, TrayStatus};
use hushmic::traylink::{self, TrayLink};
use hushmic::{autostart, lock, mictest, shortcuts, watchdog};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Unifies the event sources (tray commands, watchdog ticks, mic-test
/// completion, termination signals) into one channel so the main loop is
/// single-threaded and owns the `Controller`.
enum Event {
    Cmd(TrayCmd),
    Tick,
    MicTestDone(Result<(), String>),
    /// Put the A/B window in front of the user: sent once by a plain
    /// (flag-less) launch, and again whenever a second launch finds this
    /// instance already running and forwards itself via the show socket —
    /// carrying the caller's display variables (a `systemd --user` daemon
    /// has none, and the window must open on the caller's screen).
    ShowWindow(Vec<(String, String)>),
    /// A CLI request from the control socket; the reply goes back through
    /// the embedded sender once the outcome is known.
    Control(control::ControlReq),
    /// A report from the global-shortcuts portal worker: availability,
    /// key events, or a finished bind dialog.
    Shortcut(shortcuts::PortalEvent),
    Shutdown,
}

/// Notification summary for enable/watchdog failures (the body carries the
/// actionable enable() error).
fn fail_summary() -> String {
    tr!("notify-chain-failed-summary")
}

/// How long a freshly spawned filter-chain host gets to register its node
/// before an absent `hushmic_source` counts as "down". Registration normally
/// takes well under a second; without the grace, the status/watchdog sampled
/// right after a spawn would flash Error / respawn a healthy child.
const STARTUP_GRACE_SECS: u64 = 3;

/// One extra Tick shortly after a chain spawn. The 5 s watchdog cadence is
/// the wrong clock for healing a capture stream that another audio tool
/// re-routed at creation (issue #5: EasyEffects) — without this the A/B
/// window sits on its connecting overlay for most of a tick.
fn schedule_early_tick(tx: &mpsc::Sender<Event>) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let _ = tx.send(Event::Tick);
    });
}

fn compute_status(
    cfg: &Config,
    controller: &mut Controller,
    node_present: Option<bool>,
) -> TrayStatus {
    if !cfg.enabled {
        TrayStatus::Off
    } else if controller.is_running()
        && (node_present.unwrap_or(true) // probe failed => don't cry wolf
            || controller
                .secs_since_spawn()
                .is_some_and(|s| s < STARTUP_GRACE_SECS))
    {
        // Healthy chain: the icon reflects the processing mode. Error keeps
        // precedence via the arm below.
        match controller.mode() {
            RunMode::Suppress => TrayStatus::Active,
            RunMode::Bypass => TrayStatus::Bypass,
            RunMode::Mute => TrayStatus::Mute,
        }
    } else {
        TrayStatus::Error
    }
}

/// Acquire the single-instance lock, or exit if another hushmic already holds
/// it (a second tray + filter-chain would fight over `hushmic_source`).
/// For a plain launch (`forward_show`) the held lock is not an error but a
/// "show yourself": ping the running instance's show socket so the app-menu
/// click still ends in a visible window, then bow out.
fn acquire_single_instance(forward_show: bool) -> std::fs::File {
    match lock::try_lock(&lock::default_lock_path()) {
        Ok(Some(f)) => f,
        Ok(None) => {
            if forward_show && lock::request_show(&lock::default_show_socket_path()) {
                eprintln!("hushmic is already running; asked it to open the A/B window.");
            } else {
                eprintln!(
                    "hushmic is already running (another instance owns the lock). If both \
                     the login autostart entry and the systemd unit are enabled, disable one \
                     of them."
                );
            }
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("hushmic: could not take single-instance lock: {e}");
            std::process::exit(1);
        }
    }
}

// --- termination signals -> orderly teardown -------------------------------
//
// Without this, SIGTERM/SIGINT/SIGHUP (session logout, Ctrl+C, kill) end the
// process before `Controller::drop` runs: the previous default mic is never
// restored and the config key stays pointed at the dying `hushmic_source`.
// The handler is async-signal-safe (a single write(2) to a pre-created pipe);
// a watcher thread turns the byte into an `Event::Shutdown`.

static SHUTDOWN_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_term_signal(_sig: libc::c_int) {
    // signal-safety(7): write(2) may set errno (e.g. EPIPE once the watcher
    // thread has closed the read end); save/restore it so a signal landing
    // between a syscall and the interrupted thread's errno read can't corrupt
    // that thread's error reporting.
    unsafe {
        let saved_errno = *libc::__errno_location();
        let fd = SHUTDOWN_FD.load(Ordering::Relaxed);
        if fd >= 0 {
            libc::write(fd, b"x".as_ptr().cast(), 1);
        }
        *libc::__errno_location() = saved_errno;
    }
}

/// Install SIGTERM/SIGINT/SIGHUP handlers writing to a self-pipe; returns the
/// read end (None if installation failed — teardown then relies on Drop only).
fn install_signal_handlers() -> Option<std::fs::File> {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        eprintln!(
            "hushmic: could not set up signal handling: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    SHUTDOWN_FD.store(fds[1], Ordering::Relaxed);
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        // fn item -> pointer -> address (a direct fn-to-integer cast trips
        // clippy's function_casts_as_integer on newer toolchains)
        sa.sa_sigaction = on_term_signal as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = libc::SA_RESTART;
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
    Some(unsafe { std::fs::File::from_raw_fd(fds[0]) })
}

/// Block until a termination signal arrives (used by --enable-once).
fn wait_for_shutdown(pipe: Option<std::fs::File>) {
    match pipe {
        Some(mut p) => {
            let mut b = [0u8; 1];
            let _ = p.read(&mut b);
        }
        None => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
    }
}

/// Resolve the (raw mic, hushmic_source) node pair for the A/B window, in
/// priority order: the live link-graph trace, the tray-configured mic, then
/// the system default source. The default-source fallback matters when the
/// chain is up but not linked to any mic and the tray is on "System default":
/// without it the raw node came out empty and the window showed a misleading
/// "no microphone detected" even though a real mic exists. An empty raw node
/// (nothing resolves at all) still opens the no-device overlay.
fn resolve_ab_nodes() -> (String, String) {
    let cfg = Config::load();
    let traced = pipewire::pw_dump()
        .as_deref()
        .and_then(|d| mictest::find_feeding_node(d, "hushmic_input"));
    let raw = mictest::resolve_raw(traced, cfg.mic.as_deref(), pipewire::get_default_source());
    (raw, "hushmic_source".to_string())
}

/// Spawn a companion window as a child of the tray (same binary, given mode
/// flag: --test-window or --about). PDEATHSIG binds it to the tray: without
/// the tray there is no virtual mic, so an orphaned A/B window would only
/// show a dead device, and an About window has nothing to be about.
/// MUST be called from the main thread (PR_SET_PDEATHSIG is thread-scoped).
/// `env` overlays display variables from a forwarded show request.
fn spawn_child_window(
    mode_flag: &str,
    env: &[(String, String)],
) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let mut c = std::process::Command::new(exe);
    c.arg(mode_flag);
    for (k, v) in env {
        c.env(k, v);
    }
    unsafe {
        c.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    c.spawn()
}

/// SIGTERM a still-live A/B window child, reap it, and sweep the transient
/// WAVs its detached backend may not get to delete. Its pw children die via
/// PDEATHSIG. No-op on an already-exited (or absent) child.
fn close_ab_window(ab_window: &mut Option<(std::process::Child, Instant, bool)>) {
    if let Some((child, ..)) = ab_window.as_mut() {
        if matches!(child.try_wait(), Ok(None)) {
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
            let _ = child.wait();
            mictest::remove_recordings();
        }
        *ab_window = None;
    }
}

/// The notification-driven mic test (record 10 s, play both takes): the
/// fallback when the A/B window cannot run, and self-announcing via
/// notifications so an unexpected fallback is not confusing.
fn start_fallback_mictest(
    cfg: &Config,
    testing: &mut bool,
    mictest_cancel: &mut Option<Arc<AtomicBool>>,
    tx: &mpsc::Sender<Event>,
) {
    let dump = pipewire::pw_dump();
    let node_present = dump.as_deref().map(|d| {
        pipewire::parse_pwdump_nodes(d)
            .iter()
            .any(|s| s.name == "hushmic_source")
    });
    let start = mictest::precondition(cfg.enabled, node_present, *testing)
        .map_err(|b| b.message())
        .and_then(|()| {
            let traced = dump
                .as_deref()
                .and_then(|d| mictest::find_feeding_node(d, "hushmic_input"));
            mictest::raw_target(traced, cfg.mic.as_deref())
                .ok_or_else(|| tr!("notify-no-feeder-body"))
        });
    match start {
        Err(msg) => notify::send(
            Slot::MicTest,
            "audio-input-microphone",
            &tr!("notify-mictest-title"),
            &msg,
        ),
        Ok(raw) => {
            *testing = true;
            let flag = Arc::new(AtomicBool::new(false));
            *mictest_cancel = Some(flag.clone());
            let tx = tx.clone();
            std::thread::spawn(move || {
                // A worker that dies without reporting would leave `testing`
                // stuck forever — even a panic must become MicTestDone.
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    mictest::run(&raw, &flag)
                }))
                .unwrap_or_else(|_| Err("the mic test crashed unexpectedly".to_string()));
                let _ = tx.send(Event::MicTestDone(res));
            });
        }
    }
}

/// Client-side verbs. `config`/`devices` never exit 2: without a daemon
/// they read/write the file (a daemon starting in the same instant wins
/// with its first save — accepted, documented). `service` is purely local.
fn cli_dispatch(args: &[String]) -> (i32, String) {
    use hushmic::config_cli;
    let words: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match words.as_slice() {
        ["config", "path"] => return (0, Config::path().display().to_string()),
        ["devices"] | ["devices", "--json"] => {
            let json = words.len() == 2;
            // The running daemon's view when there is one (its config is
            // the truth while it runs), else the file.
            let status: Vec<String> = ["status", "--json"].iter().map(|s| s.to_string()).collect();
            let configured = match control::client_run(&status) {
                (0, out) => serde_json::from_str::<serde_json::Value>(&out)
                    .ok()
                    .and_then(|v| v["mic"]["configured"].as_str().map(str::to_string)),
                _ => Config::load().mic,
            };
            let sources = pipewire::list_real_sources();
            return (
                0,
                config_cli::render_devices(&sources, configured.as_deref(), json),
            );
        }
        ["devices", ..] => return (1, "usage: hushmic devices [--json]".into()),
        ["service", "install"] => {
            let mut msg = match hushmic::service::install() {
                Ok(m) => m,
                Err(e) => return (1, e),
            };
            // The unit replaces the login autostart entry (both enabled
            // would race for the lock at every login). Through the daemon
            // when one runs — it owns the file — else on the file.
            if Config::load().autostart {
                let set: Vec<String> = ["config", "set", "autostart", "false"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let (code, out) = control::client_run(&set);
                let line = match code {
                    0 => out,
                    2 => config_cli::offline_set("autostart", "false", &Paths::resolve().model_dir)
                        .unwrap_or_else(|e| e),
                    _ => out,
                };
                msg.push_str(&format!(
                    "{} (the unit replaces the login autostart entry)\n",
                    line.trim_end()
                ));
            }
            return (0, msg);
        }
        ["service", "uninstall"] => {
            return match hushmic::service::uninstall() {
                Ok(m) => (0, m),
                Err(e) => (1, e),
            }
        }
        ["service", ..] => return (1, "usage: hushmic service install|uninstall".into()),
        _ => {}
    }
    let (code, out) = control::client_run(args);
    if code == 0 && matches!(words.as_slice(), ["config", "set", "autostart", ..]) {
        // Both the entry and the unit enabled would race for the lock at
        // every login. Checked here, not in the daemon: `systemctl` is a
        // synchronous child and the daemon's loop must not wait on it.
        if !hushmic::sandbox::is_flatpak()
            && Config::load().autostart
            && hushmic::service::unit_enabled()
        {
            return (
                0,
                format!(
                    "{} (note: the systemd unit hushmic.service is enabled too; use one or the other)",
                    out.trim_end()
                ),
            );
        }
        return (code, out);
    }
    if code != 2 || words.first() != Some(&"config") {
        return (code, out);
    }
    // No daemon: the file is the truth.
    let model_dir = Paths::resolve().model_dir;
    let cfg = Config::load();
    let res = match words.as_slice() {
        ["config"] => config_cli::offline_get(&cfg, None, false),
        ["config", "--json"] => config_cli::offline_get(&cfg, None, true),
        ["config", "get", k] => config_cli::offline_get(&cfg, Some(k), false),
        ["config", "get", k, "--json"] => config_cli::offline_get(&cfg, Some(k), true),
        ["config", "set", k, rest @ ..] if !rest.is_empty() => {
            config_cli::offline_set(k, &rest.join(" "), &model_dir)
        }
        _ => Err(out),
    };
    match res {
        Ok(s) => (0, s),
        Err(e) => (1, e),
    }
}

/// The one path that changes settings from outside the tray menu: diff,
/// apply only what changed, persist, and only then report. `cfg` is
/// replaced by `new` even when the chain restart fails (the tray does the
/// same: a bad setting is visible, not silently reverted). Every error
/// (autostart entry, chain restart, save) is reported, joined with "; ".
#[allow(clippy::too_many_arguments)]
fn apply_config(
    cfg: &mut Config,
    new: Config,
    controller: &mut Controller,
    apply: &dyn Fn(&mut Controller, &Config) -> Result<(), String>,
    testing: bool,
    mictest_cancel: &Option<Arc<AtomicBool>>,
    ab_window: &mut Option<(std::process::Child, Instant, bool)>,
    gate: &mut FailureGate,
) -> Result<(), String> {
    let d = hushmic::config_cli::diff(cfg, &new);
    if d.chain && cfg.enabled {
        // Same invalidation as the tray commands: a running mic test would
        // record a chain mid-restart, and the A/B window compares the old
        // mic against the new output.
        if testing {
            if let Some(c) = mictest_cancel {
                c.store(true, Ordering::Relaxed);
            }
        }
        close_ab_window(ab_window);
    }
    let mut errors: Vec<String> = Vec::new();
    if d.notifications {
        notify::set_enabled(new.notifications);
    }
    if d.autostart {
        if let Err(e) = autostart::set_autostart(new.autostart) {
            errors.push(format!("the autostart entry could not be written: {e}"));
        }
    }
    let chain_result = if d.chain && new.enabled {
        apply(controller, &new)
    } else {
        Ok(())
    };
    *cfg = new;
    if let Err(e) = &chain_result {
        eprintln!("hushmic: enable failed: {e}");
        if gate.on_enable_error(e, true) {
            notify::send(Slot::Status, "dialog-error", &fail_summary(), e);
        }
        errors.push(format!("the microphone could not restart: {e}"));
    }
    if let Err(e) = cfg.save() {
        errors.push(format!("could not write {}: {e}", Config::path().display()));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// The two engine fields the tray shows, as one value: the tier the
/// running chain reports (None without a chain) and whether that chain is
/// on the light model by configuration, which decides whether a reported
/// `light` reads as a fallback. They travel together because the title and
/// the model menu read both.
type EngineView = (Option<hushmic::diagnostics::EngineTier>, bool);

fn engine_view(controller: &mut Controller) -> EngineView {
    if controller.is_running() {
        (
            hushmic::diagnostics::engine_tier(),
            controller.active_model_is_light(),
        )
    } else {
        (None, false)
    }
}

/// What the tray is known to be displaying for the engine, or None when
/// that is unknown: no tray registered yet, or an update that was dropped
/// because there was none. Keeping "unknown" distinct from "nothing
/// reported" is the point: an update the tray never received must not be
/// remembered as delivered, or a tray that registers later shows the
/// state from before it existed until the tier happens to change again.
#[derive(Default)]
struct EngineCache(Option<EngineView>);

impl EngineCache {
    /// Push the engine fields when they differ from what the tray shows.
    /// `send` reports whether the update reached a tray.
    fn push_changed(&mut self, now: EngineView, send: impl FnOnce(EngineView) -> bool) {
        if self.0 == Some(now) {
            return;
        }
        self.record(now, send(now));
    }
    /// Note what a full refresh just sent, and whether it landed.
    fn record(&mut self, now: EngineView, landed: bool) {
        self.0 = landed.then_some(now);
    }
}

/// After any settings change (tray menu or `config set`): one pw-dump
/// snapshot refreshes the mic list and the node probe, and the tray gets
/// the full state. The one place the post-change tray closure lives.
fn refresh_tray(
    handle: &TrayLink,
    cfg: &Config,
    controller: &mut Controller,
    known_mics: &mut Vec<pipewire::Source>,
    last_node_present: &mut Option<bool>,
    engine: &mut EngineCache,
    testing: bool,
) {
    let nodes = pipewire::sources_snapshot();
    let node_present = nodes
        .as_ref()
        .map(|v| v.iter().any(|s| s.name == "hushmic_source"));
    if let Some(v) = nodes {
        *known_mics = pipewire::filter_real(&v);
    }
    *last_node_present = node_present;
    let status = compute_status(cfg, controller, node_present);
    let new_mics = known_mics.clone();
    let snapshot = cfg.clone();
    let fallback_now = cfg.enabled
        && cfg.mic.is_some()
        && controller.is_running()
        && controller.active_mic() != cfg.mic.as_deref();
    let mode_now = controller.mode();
    // A full refresh carries the engine fields too: a restart or a model
    // change can move the tier and the configured-light flag at the same
    // time, and a tray that registers here has never seen either.
    let engine_now = engine_view(controller);
    // `tray_icon` is resolved here too, so `config set tray_icon` repaints
    // the icon on this very refresh instead of on the next start.
    let icon_style = hushmic::tray::icon_style_for(cfg);
    let landed = handle
        .update(move |t: &mut HushMicTray| {
            t.cfg = snapshot;
            t.icon_style = icon_style;
            t.mics = new_mics;
            t.status = status;
            t.testing = testing;
            t.fallback_active = fallback_now;
            t.mode = mode_now;
            t.engine = engine_now.0;
            t.engine_light_configured = engine_now.1;
        })
        .is_some();
    engine.record(engine_now, landed);
}

fn usage_text() -> String {
    "usage: hushmic                 start the tray and open the A/B window
                               (an already-running instance opens it instead)
       hushmic --tray          run the system-tray app (no window; autostart)
       hushmic --headless      run without a tray icon or window (systemd unit)
       hushmic --enable-once   enable the mic until terminated (no watchdog, no CLI socket)
       hushmic --version       print the version and install paths
       hushmic --doctor        print a diagnostics report (exits 1 on problems)
       hushmic --help          this text

control (talks to the running instance):
       hushmic status [--json]      what it is doing
       hushmic mode [STATE]         print or set: suppress|bypass|mute|off
       hushmic toggle mute|bypass   hotkey-friendly overlay toggle
       hushmic quit                 stop it (restores the previous default mic)

settings (applied live while running, saved to the file otherwise):
       hushmic config [--json]           every key = value
       hushmic config get KEY [--json]
       hushmic config set KEY VALUE
       hushmic config path
       hushmic devices [--json]          microphones usable as `mic`
       hushmic service install|uninstall systemd user unit for this install
  keys: mic model attn_limit set_default autostart tray tray_icon notifications

exit codes: 0 ok, 1 usage/failed, 2 not running (control commands only)
"
    .to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(|s| s.as_str()), Some("--help" | "-h")) && args.len() == 1 {
        use std::io::Write;
        let _ = std::io::stdout().write_all(usage_text().as_bytes());
        return;
    }
    // Control subcommands are CLIENT invocations: talk to the running
    // tray's socket, print, exit. Same SIGPIPE stance as --version.
    if matches!(
        args.first().map(|s| s.as_str()),
        Some("status" | "mode" | "toggle" | "quit" | "config" | "devices" | "service")
    ) {
        use std::io::Write;
        let (code, out) = cli_dispatch(&args);
        let text = if out.ends_with('\n') {
            out
        } else {
            format!("{out}\n")
        };
        let res = if code == 0 {
            std::io::stdout().write_all(text.as_bytes())
        } else {
            std::io::stderr().write_all(text.as_bytes())
        };
        if let Err(e) = res {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                eprintln!("hushmic: cannot write output: {e}");
            }
        }
        std::process::exit(code);
    }
    let tray_mode = args.iter().any(|a| a == "--tray");
    let headless = args.iter().any(|a| a == "--headless");
    let enable_once = args.iter().any(|a| a == "--enable-once");
    let test_window = args.iter().any(|a| a == "--test-window");
    let about = args.iter().any(|a| a == "--about");
    let version = args.iter().any(|a| a == "--version");
    let doctor = args.iter().any(|a| a == "--doctor");
    const KNOWN_FLAGS: [&str; 7] = [
        "--tray",
        "--headless",
        "--enable-once",
        "--test-window",
        "--about",
        "--version",
        "--doctor",
    ];
    let unrecognized = args.iter().any(|a| !KNOWN_FLAGS.contains(&a.as_str()));
    let modes = [
        tray_mode,
        headless,
        enable_once,
        test_window,
        about,
        version,
        doctor,
    ]
    .iter()
    .filter(|m| **m)
    .count();
    if unrecognized || modes > 1 {
        eprint!("{}", usage_text());
        std::process::exit(2);
    }
    // No flag at all is the DESKTOP LAUNCH: run the tray AND surface the A/B
    // window, so clicking the app icon always produces something visible
    // (Flathub rejects tray-only launchers, and it is better UX everywhere).
    // The desktop entry uses it; autostart keeps `--tray` for the silent path.
    let show_mode = modes == 0;

    if version {
        // Best-effort install facts: Paths::resolve() is the same cheap
        // env/prefix probe enable() uses, so what prints here is exactly
        // what the app would load.
        //
        // Written via write! instead of println!: the Rust runtime ignores
        // SIGPIPE, so `hushmic --version | head -1` surfaces the closed pipe
        // as an EPIPE error that println! turns into a panic. A quiet exit is
        // the correct CLI behavior. Deliberately NOT fixed by resetting
        // SIGPIPE to SIG_DFL process-wide: the tray is long-running, and a
        // journald restart closing its stdout must not kill it.
        use std::io::Write;
        let paths = Paths::resolve();
        let out = format!(
            "hushmic {}\nconfig: {}\nplugin: {}\nmodels: {}\n",
            env!("CARGO_PKG_VERSION"),
            Config::path().display(),
            paths.plugin_so.display(),
            paths.model_dir.display()
        );
        if let Err(e) = std::io::stdout().write_all(out.as_bytes()) {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                eprintln!("hushmic: cannot write to stdout: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if doctor {
        // Same SIGPIPE stance as --version: `hushmic --doctor | head` must
        // exit quietly, not panic.
        use std::io::Write;
        let (text, problems) = {
            let report = hushmic::diagnostics::collect();
            hushmic::diagnostics::render(&report)
        };
        if let Err(e) = std::io::stdout().write_all(text.as_bytes()) {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                eprintln!("hushmic: cannot write to stdout: {e}");
                std::process::exit(1);
            }
        }
        std::process::exit(if problems == 0 { 0 } else { 1 });
    }

    if about {
        // Companion window to the tray (or standalone): like --test-window,
        // no single-instance lock and no signal plumbing — closing the
        // window is the teardown.
        if let Err(e) = hushmic::about::run() {
            eprintln!("hushmic: about window failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    if test_window {
        // The child sends its own notifications: honour the same switch.
        notify::set_enabled(Config::load().notifications);
        // pw-cat before the mid-2022 rework (Ubuntu 22.04 ships 0.3.48) cannot
        // stream a capture to a pipe — which is how the live view reads audio —
        // so the A/B window can only sit at −∞ there. Explain and exit 1: the
        // tray then runs the file-based recording test (pw-cat writes a real
        // file fine on every version), reusing the same path as the no-GL
        // fallback. Standalone `--test-window` just prints the reason and exits.
        if !pipewire::supports_pipe_capture() {
            eprintln!("hushmic: The live A/B view needs a newer PipeWire on this system.");
            // Bounded wait, not fire-and-forget: the detached send worker dies
            // with the process on the exit below and the notification would be
            // lost (same reason main()'s could-not-start path uses this).
            notify::send_and_wait(
                Slot::MicTest,
                "audio-input-microphone",
                &tr!("notify-mictest-title"),
                &tr!("notify-old-pipewire-body"),
                Duration::from_secs(2),
            );
            std::process::exit(1);
        }
        // Companion window to a RUNNING tray instance: no single-instance
        // lock (it owns no mic), no signal plumbing (closing the window is
        // the teardown; children die via PDEATHSIG on abnormal exit).
        let (raw, filtered) = resolve_ab_nodes();
        let result = hushmic::abtest::run_window(raw, filtered);
        // The backend thread's own on-close WAV deletion is detached and
        // races process exit (it loses whenever a sample is playing):
        // sweep synchronously before returning — idempotent with it.
        hushmic::mictest::remove_recordings();
        if let Err(e) = result {
            eprintln!("hushmic: test window failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    let _lock = acquire_single_instance(show_mode);
    let shutdown_pipe = install_signal_handlers();

    // Bind the show socket the moment we own the lock (--enable-once opts
    // out: it has no window to show). Binding cannot wait until the event
    // loop is wired up: the tray-registration retry below can take up to a
    // minute at login, and a plain `hushmic` clicked in that window would
    // find the lock held but nobody listening — its request silently lost
    // (worst on an autostarted `--tray`, which never opens a window by
    // itself). With the listener bound, such connects queue in the kernel
    // backlog until the forwarding thread starts accepting.
    let show_listener = if enable_once {
        None
    } else {
        match lock::bind_show_socket(&lock::default_show_socket_path()) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("hushmic: relaunch forwarding disabled: {e}");
                None
            }
        }
    };
    // The CLI control socket, bound equally early for the same backlog
    // reason: a `hushmic mode mute` racing the tray's startup should queue,
    // not exit 2.
    let control_socket_path = lock::default_control_socket_path();
    let control_listener = if enable_once {
        None
    } else {
        match lock::bind_control_socket(&control_socket_path) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("hushmic: CLI control disabled: {e}");
                None
            }
        }
    };

    // If a previous run died without restoring the default mic (crash,
    // SIGKILL, power loss), repair it before doing anything else.
    controller::recover_dangling_default();

    if enable_once {
        // Scripted/integration use: enable, then hold until terminated. The
        // Drop teardown restores the previous default and reaps the child.
        let mut c = Controller::new(Paths::resolve());
        if let Err(e) = c.enable(&Config::load()) {
            eprintln!("hushmic: enable failed: {e}");
            std::process::exit(1);
        }
        // enable() succeeding only means the child SPAWNED. Headless there is
        // no watchdog or notification to catch a node that never registers
        // (e.g. PipeWire not running), so verify before claiming success —
        // otherwise scripts hang on a mic that will never exist. The wait
        // returns as soon as the node shows up; 5 s is only the failure path.
        if !pipewire::wait_for_hushmic_source(std::time::Duration::from_secs(5)) {
            eprintln!(
                "hushmic: enable failed: hushmic_source never appeared (is PipeWire running?)"
            );
            // Explicit drop, not bare exit: Drop reaps the child and
            // restores/clears the default source we may have taken.
            drop(c);
            std::process::exit(1);
        }
        eprintln!("hushmic: enabled; send SIGTERM or press Ctrl+C to stop.");
        wait_for_shutdown(shutdown_pipe);
        drop(c);
        return;
    }

    let mut cfg = Config::load();
    notify::set_enabled(cfg.notifications);
    let mut controller = Controller::new(Paths::resolve());
    // For `config set model` validation: the same directory enable() reads.
    let model_dir = Paths::resolve().model_dir;

    // A previous run that died mid-mic-test never got to delete its
    // recordings — the user's voice must not linger on disk.
    mictest::remove_recordings();

    let (tx, rx) = mpsc::channel::<Event>();

    // Tray -> commands
    let (ctx, crx) = mpsc::channel::<TrayCmd>();
    let mut known_mics = pipewire::list_real_sources();
    let want_tray = !headless && cfg.tray;
    if !want_tray {
        eprintln!(
            "hushmic: running without a tray icon ({})",
            if headless {
                "--headless"
            } else {
                "config.toml tray = false"
            }
        );
        if !cfg.enabled {
            eprintln!("hushmic: mode: off — run 'hushmic mode suppress' to start the microphone");
        }
    }
    // One registration attempt now; a missing StatusNotifierWatcher is not
    // fatal. At login we can outrun the desktop's watcher (Cinnamon's
    // xapp-sn-watcher only registers once the applets load and is not
    // DBus-activatable — reproduced 3x on Mint 22.1), and stock GNOME has
    // none at all: both keep the microphone running and get the icon on a
    // later Tick (see there). The startup snapshot is Off/empty; the launch
    // block below and every Tick push the real state.
    let make_tray = |cfg: &Config, mics: &[pipewire::Source]| HushMicTray {
        cfg: cfg.clone(),
        mics: mics.to_vec(),
        cmd_tx: ctx.clone(),
        status: TrayStatus::Off,
        testing: false,
        fallback_active: false,
        mode: RunMode::default(),
        shortcuts_available: false,
        engine: None,
        engine_light_configured: false,
        icon_style: hushmic::tray::icon_style_for(cfg),
    };
    let mut handle = if !want_tray {
        TrayLink::Disabled
    } else {
        match traylink::try_spawn(make_tray(&cfg, &known_mics)) {
            Ok(h) => h,
            Err(e) => {
                eprintln!(
                    "hushmic: no system tray yet ({e}); running without an icon and retrying"
                );
                TrayLink::Pending
            }
        }
    };
    let tray_wanted_at = Instant::now();
    let mut tray_backoff = watchdog::Backoff::new();
    let mut tray_notified = false;

    // bridge TrayCmd -> Event
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for c in crx {
                if tx.send(Event::Cmd(c)).is_err() {
                    break;
                }
            }
        });
    }
    // watchdog -> Event::Tick
    {
        let (wtx, wrx) = mpsc::channel::<watchdog::Tick>();
        watchdog::spawn(wtx, 5);
        let tx = tx.clone();
        std::thread::spawn(move || {
            for _ in wrx {
                if tx.send(Event::Tick).is_err() {
                    break;
                }
            }
        });
    }
    // termination signal -> Event::Shutdown
    if let Some(mut p) = shutdown_pipe {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 1];
            let _ = p.read(&mut b);
            let _ = tx.send(Event::Shutdown);
        });
    }
    // relaunch forwarding -> Event::ShowWindow: a plain `hushmic` that finds
    // our lock held connects to the socket bound right after lock
    // acquisition (see there) instead of starting anything; each accepted
    // connection is one "open the window" request, including any that
    // queued in the backlog while the tray was still registering.
    if let Some(listener) = show_listener {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { break };
                let env = lock::read_show_line(&conn);
                if tx.send(Event::ShowWindow(env)).is_err() {
                    break;
                }
            }
        });
    }
    // Control listener + a small bridge into the event channel (the
    // listener speaks ControlReq, the loop speaks Event).
    if let Some(listener) = control_listener {
        let (ctl_tx, ctl_rx) = mpsc::channel::<control::ControlReq>();
        control::spawn_listener(listener, ctl_tx);
        let tx = tx.clone();
        std::thread::spawn(move || {
            for req in ctl_rx {
                if tx.send(Event::Control(req)).is_err() {
                    break;
                }
            }
        });
    }
    // Global-shortcuts portal worker + the same bridge shape into the
    // event channel. Spawned unconditionally: on a desktop without the
    // portal it reports Unavailable once and then only speaks when the
    // Tick arm nudges it (backoff-gated), so there is no hot loop.
    let shortcuts_cmd = {
        let (sc_tx, sc_rx) = mpsc::channel::<shortcuts::PortalEvent>();
        let worker = shortcuts::spawn(sc_tx, cfg.shortcuts_setup);
        let tx = tx.clone();
        std::thread::spawn(move || {
            for pe in sc_rx {
                if tx.send(Event::Shortcut(pe)).is_err() {
                    break;
                }
            }
        });
        worker
    };

    // Gate for failure/recovery notifications: dedups the watchdog's
    // backoff-gated retries so the same error does not re-pop forever.
    let mut gate = FailureGate::new();

    // apply persisted state on launch
    let _ = autostart::reconcile(cfg.autostart);
    if cfg.enabled {
        match controller.enable(&cfg) {
            Ok(()) => schedule_early_tick(&tx),
            Err(e) => {
                eprintln!("hushmic: enable failed: {e}");
                // A launch failure is invisible on stderr for
                // .desktop/autostart starts — surface it (launch counts as
                // user-initiated).
                if gate.on_enable_error(&e.to_string(), true) {
                    notify::send(
                        Slot::Status,
                        "dialog-error",
                        &fail_summary(),
                        &e.to_string(),
                    );
                }
            }
        }
    }
    // Reflect the launch-time state immediately: the tray was registered as Off
    // above, and the first watchdog tick is 5 s out.
    {
        let status = compute_status(&cfg, &mut controller, pipewire::hushmic_source_present());
        let _ = handle.update(move |t: &mut HushMicTray| {
            t.status = status;
        });
    }

    // Errors bubble up as the enable() message so the caller can notify;
    // disable() practically cannot fail and is not notification-worthy.
    let apply = |controller: &mut Controller, cfg: &Config| -> Result<(), String> {
        if cfg.enabled {
            controller.enable(cfg).map_err(|e| e.to_string())
        } else {
            if let Err(e) = controller.disable() {
                eprintln!("hushmic: disable failed: {e}");
            }
            Ok(())
        }
    };

    // watchdog respawn backoff + throttled "node down" logging (state-change only)
    let mut backoff = hushmic::watchdog::Backoff::new();
    let mut logged_down = false;
    let mut recovery = watchdog::Recovery::new();
    // A mic test is running (worker thread active). Owned by the main loop:
    // set on TrayCmd::TestMic, cleared on Event::MicTestDone. The flag lets
    // the loop cancel the test when the filter-chain is mutated under it.
    let mut testing = false;
    let mut mictest_cancel: Option<Arc<AtomicBool>> = None;
    // The spawned A/B test window child (PDEATHSIG-bound to this process).
    // The bool records whether the USER asked for a MIC TEST (tray click) —
    // only that path may escalate to the audio-only fallback recording when
    // the window cannot start (no GL, headless). Launch-driven windows
    // (plain `hushmic`, relaunch forwarding) must not: they would turn
    // "open the app" into an unsolicited microphone recording.
    let mut ab_window: Option<(std::process::Child, Instant, bool)> = None;
    // A plain launch ends in a visible window: the A/B view doubles as the
    // best possible "it's working" moment, and the desktop entry counts on
    // it (the store rejects tray-only launchers). Queue it through the same
    // handler a forwarded relaunch uses; the wait gives a fresh chain a beat
    // to register so the window resolves real nodes instead of opening on
    // the no-device overlay.
    if show_mode {
        if cfg.enabled {
            let _ = pipewire::wait_for_hushmic_source(Duration::from_secs(2));
        }
        let _ = tx.send(Event::ShowWindow(Vec::new()));
    }
    // Spawned About window children. Multiple are acceptable (each click just
    // opens another), but every one must be reaped on Tick or it lingers as a
    // zombie after close.
    let mut about_windows: Vec<std::process::Child> = Vec::new();
    // The toggle overlay's return address: the last chain-alive mode before
    // the current one, updated on EVERY SetMode (tray radio or CLI) so
    // tray-mute then CLI-untoggle round-trips. See control::update_prev_alive.
    let mut prev_alive = RunMode::Suppress;
    // Last hushmic_source probe verdict, refreshed by Tick and the Cmd
    // epilogue — `status` reports it instead of re-probing on the hot path.
    let mut last_node_present: Option<bool> = None;
    let mut tray_engine = EngineCache::default();
    // Metadata stream routing (the re-pin write) exists only on modern
    // PipeWire; probed once — on legacy hosts the theft mechanism does not
    // exist either, and inert writes would just spam the log.
    let repin_supported = pipewire::supports_target_object();
    // Consecutive ticks the capture stream was observed stolen, the time
    // of the last correction, and whether the tug-of-war notice went out
    // (once per run) — see pipewire::repin_allowed.
    let mut repin_streak: u32 = 0;
    let mut repin_last: Option<Instant> = None;
    let mut repin_notified = false;
    // Shortcuts portal state: None until the worker's first report;
    // Some(false) makes Tick nudge a reconnect, throttled by the backoff.
    let mut shortcuts_up: Option<bool> = None;
    let mut shortcuts_backoff = watchdog::Backoff::new();
    // What the hold keys are doing (push-to-mute's armed release,
    // push-to-talk's dangling-hold re-mute) — see shortcuts::Holds.
    let mut shortcut_holds = shortcuts::Holds::default();

    for ev in rx {
        // Mutating control requests become synthetic SetMode commands so
        // they share the entire Cmd path (mic-test invalidation, live
        // switch + restart fallback, config save, tray refresh); the reply
        // is written from the Cmd epilogue once the outcome is known.
        // Read-only requests answer inline from loop-owned state.
        let mut control_reply: Option<mpsc::Sender<String>> = None;
        let ev = match ev {
            Event::Control(req) => {
                let words: Vec<&str> = req.line.split_whitespace().collect();
                match control::parse_request(&words) {
                    Err(msg) => {
                        let _ = req.reply.send(control::encode_err(&msg));
                        continue;
                    }
                    Ok(control::Request::GetMode) => {
                        let cur = cfg.enabled.then(|| controller.mode());
                        let _ = req.reply.send(control::encode_ok(control::mode_word(cur)));
                        continue;
                    }
                    Ok(control::Request::Status { json }) => {
                        let s = control::Status {
                            version: env!("CARGO_PKG_VERSION").to_string(),
                            mode: cfg.enabled.then(|| controller.mode()),
                            mic_configured: cfg.mic.clone(),
                            mic_active: controller.active_mic().map(str::to_string),
                            fallback_active: cfg.enabled
                                && cfg.mic.is_some()
                                && controller.is_running()
                                && controller.active_mic() != cfg.mic.as_deref(),
                            model: cfg.model.clone(),
                            attn_limit: cfg.attn_limit,
                            chain_running: controller.is_running(),
                            node_present: last_node_present,
                            tray_sni: handle.is_sni(),
                            engine: controller
                                .is_running()
                                .then(hushmic::diagnostics::engine_tier)
                                .flatten(),
                            configured_light: controller.active_model_is_light(),
                        };
                        let payload = if json {
                            control::render_status_json(&s)
                        } else {
                            control::render_status_human(&s)
                        };
                        let _ = req.reply.send(control::encode_ok(&payload));
                        continue;
                    }
                    Ok(control::Request::Quit) => {
                        // Answer first, and wait until the bytes are on the
                        // wire: the Quit arm breaks the loop and the process
                        // exits right after — with the chain already off,
                        // that is microseconds away.
                        let _ = req.reply.send(control::encode_ok("stopping"));
                        let _ = req.written.recv_timeout(Duration::from_secs(2));
                        Event::Cmd(TrayCmd::Quit)
                    }
                    Ok(control::Request::ConfigGet { key, json }) => {
                        let msg = match hushmic::config_cli::offline_get(&cfg, key.as_deref(), json)
                        {
                            Ok(s) => control::encode_ok(&s),
                            Err(e) => control::encode_err(&e),
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    Ok(control::Request::ConfigSet { key, value }) => {
                        use hushmic::config_cli as cc;
                        // The value as typed (the parser's word list
                        // collapsed interior whitespace).
                        let value = control::config_set_value(&req.line).unwrap_or(value);
                        // Validate before touching anything: a rejected
                        // value leaves cfg, the chain and the file alone.
                        let parsed = cc::parse_settable_key(&key)
                            .and_then(|k| cc::parse_value(k, &value, &model_dir).map(|v| (k, v)));
                        let (k, v) = match parsed {
                            Ok(kv) => kv,
                            Err(e) => {
                                let _ = req.reply.send(control::encode_err(&e));
                                continue;
                            }
                        };
                        let mut new = cfg.clone();
                        cc::apply(&mut new, k, v);
                        let chain_changed = cc::diff(&cfg, &new).chain;
                        let res = apply_config(
                            &mut cfg,
                            new,
                            &mut controller,
                            &apply,
                            testing,
                            &mictest_cancel,
                            &mut ab_window,
                            &mut gate,
                        );
                        let mut line = format!(
                            "{} = {}{}",
                            k.name(),
                            cc::get(&cfg, k),
                            cc::set_qualifier(k, true)
                        );
                        if matches!(k, cc::Key::Tray | cc::Key::TrayIcon) && headless {
                            line.push_str(" (--headless ignores it)");
                        }
                        if k == cc::Key::Autostart && hushmic::sandbox::is_flatpak() {
                            line.push_str(" (requested; the desktop decides)");
                        }
                        let msg = match res {
                            Ok(()) => control::encode_ok(&line),
                            Err(e) => control::encode_err(&format!("{line}; {e}")),
                        };
                        let _ = req.reply.send(msg);
                        if chain_changed && cfg.enabled {
                            // A restart may have respawned the chain: heal
                            // a stolen capture stream before the next tick.
                            schedule_early_tick(&tx);
                        }
                        // The tray reflects the change like after a menu
                        // click.
                        refresh_tray(
                            &handle,
                            &cfg,
                            &mut controller,
                            &mut known_mics,
                            &mut last_node_present,
                            &mut tray_engine,
                            testing,
                        );
                        continue;
                    }
                    Ok(control::Request::SetMode(sel)) => {
                        control_reply = Some(req.reply);
                        Event::Cmd(TrayCmd::SetMode(sel))
                    }
                    Ok(control::Request::Toggle(target)) => {
                        let cur = cfg.enabled.then(|| controller.mode());
                        let sel = control::toggle_next(cur, prev_alive, target);
                        control_reply = Some(req.reply);
                        Event::Cmd(TrayCmd::SetMode(sel))
                    }
                }
            }
            // Shortcut key events run the press/release machine and, when
            // it selects a mode, become synthetic SetMode commands — the
            // same one state machine as the tray radio and the CLI.
            // Housekeeping reports are absorbed here.
            Event::Shortcut(pe) => {
                use shortcuts::PortalEvent as PE;
                let transition = |a, activated: bool, holds: &mut shortcuts::Holds| {
                    let cur = cfg.enabled.then(|| controller.mode());
                    shortcuts::shortcut_transition(a, activated, cur, prev_alive, holds)
                };
                match pe {
                    PE::Available => {
                        shortcuts_up = Some(true);
                        shortcuts_backoff.record(true);
                        let _ = handle.update(|t: &mut HushMicTray| t.shortcuts_available = true);
                        continue;
                    }
                    PE::Unavailable => {
                        shortcuts_up = Some(false);
                        shortcuts_backoff.record(false);
                        let _ = handle.update(|t: &mut HushMicTray| t.shortcuts_available = false);
                        // A session death mid-push-to-talk loses the
                        // release forever — the key promised "live only
                        // while held", so err toward muted and re-mute.
                        let ptt_dangling = shortcut_holds.ptt_held();
                        shortcut_holds.reset();
                        if ptt_dangling && cfg.enabled && controller.mode() != RunMode::Mute {
                            eprintln!("[hushmic] portal session died mid push-to-talk: muting");
                            Event::Cmd(TrayCmd::SetMode(Some(RunMode::Mute)))
                        } else {
                            continue;
                        }
                    }
                    PE::BindDone => {
                        // The compositor's dialog returned: from now on
                        // every start re-binds silently so events flow,
                        // and the menu entry becomes "Change shortcuts…".
                        cfg.shortcuts_setup = true;
                        if let Err(e) = cfg.save() {
                            // Unsaved, the next start neither re-binds
                            // silently nor shows "Change shortcuts…" —
                            // leave the one user hitting this a trace.
                            eprintln!("[hushmic] could not persist shortcuts_setup: {e}");
                        }
                        let snapshot = cfg.clone();
                        let _ = handle.update(move |t: &mut HushMicTray| t.cfg = snapshot);
                        continue;
                    }
                    PE::ConfigureUnavailable => {
                        // No ConfigureShortcuts on this portal (v1) or the
                        // call failed: a click that silently does nothing
                        // is the one thing this entry must never be.
                        notify::send(
                            Slot::Status,
                            "preferences-desktop-keyboard",
                            &tr!("notify-shortcuts-title"),
                            &tr!("notify-shortcuts-body"),
                        );
                        continue;
                    }
                    PE::Activated(a) => match transition(a, true, &mut shortcut_holds) {
                        Some(sel) => Event::Cmd(TrayCmd::SetMode(sel)),
                        None => continue,
                    },
                    PE::Deactivated(a) => match transition(a, false, &mut shortcut_holds) {
                        Some(sel) => Event::Cmd(TrayCmd::SetMode(sel)),
                        None => continue,
                    },
                }
            }
            other => other,
        };
        match ev {
            Event::Cmd(cmd) => {
                // Any command that will re-render/restart the chain (or tear
                // it down) invalidates a running mic test's cleaned leg —
                // cancel it rather than let it record a dead node.
                // A live mode switch (SetMode(Some) with a running chain)
                // deliberately does NOT count: it flips a control on the
                // running node without restarting anything, and the A/B
                // window showing the flip live is the honest behavior. Its
                // rare set-param-failed fallback DOES restart — that path
                // re-runs this invalidation inline before applying.
                let mutates_chain = matches!(cmd, TrayCmd::SetMode(None))
                    || (!cfg.enabled && matches!(cmd, TrayCmd::SetMode(Some(_))))
                    || (cfg.enabled
                        && matches!(
                            cmd,
                            TrayCmd::SelectMic(_)
                                | TrayCmd::SelectModel(_)
                                | TrayCmd::SetAttn(_)
                                | TrayCmd::SetDefaultToggle(_)
                        ));
                if testing && mutates_chain {
                    if let Some(c) = &mictest_cancel {
                        c.store(true, Ordering::Relaxed);
                    }
                }
                // The A/B window resolves its raw node once at spawn: after
                // a chain mutation it would silently compare the OLD mic
                // against the NEW output (and "already open" would steer
                // the user back to it). Close it — reopening gets a fresh
                // trace.
                if mutates_chain {
                    close_ab_window(&mut ab_window);
                }
                let mut applied: Result<(), String> = Ok(());
                match cmd {
                    TrayCmd::SetMode(sel) => {
                        let old_sel = cfg.enabled.then(|| controller.mode());
                        match sel {
                            None => {
                                cfg.enabled = false;
                                applied = apply(&mut controller, &cfg);
                            }
                            Some(m) => {
                                let live = cfg.enabled && controller.is_running();
                                controller.set_mode_state(m);
                                cfg.enabled = true;
                                if live && pipewire::set_chain_mode(m.control_value()) {
                                    // switched on the running node - no restart,
                                    // no audible gap, nothing else to do
                                } else {
                                    if live {
                                        eprintln!(
                                            "[hushmic] live mode switch failed; \
                                         restarting the chain with mode {m:?}"
                                        );
                                        // the restart invalidates what the
                                        // mutates_chain gate above skipped for
                                        // the live path
                                        if testing {
                                            if let Some(c) = &mictest_cancel {
                                                c.store(true, Ordering::Relaxed);
                                            }
                                        }
                                        close_ab_window(&mut ab_window);
                                    }
                                    applied = apply(&mut controller, &cfg);
                                }
                            }
                        }
                        prev_alive = control::update_prev_alive(prev_alive, old_sel, sel);
                    }
                    TrayCmd::SelectMic(m) => {
                        // Loads the pick's saved profile into model/attn
                        // (per-mic prefs); the snapshot pushed back below
                        // updates the tray radios to match.
                        cfg.apply_mic_selection(m);
                        if cfg.enabled {
                            applied = apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SelectModel(m) => {
                        cfg.model = m;
                        cfg.remember_selected_prefs();
                        if cfg.enabled {
                            applied = apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SetAttn(v) => {
                        cfg.attn_limit = v;
                        cfg.remember_selected_prefs();
                        if cfg.enabled {
                            applied = apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SetDefaultToggle(v) => {
                        cfg.set_default = v;
                        if cfg.enabled {
                            applied = apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SetAutostart(v) => {
                        cfg.autostart = v;
                        let _ = autostart::set_autostart(v);
                    }
                    TrayCmd::TestMic => {
                        let window_alive = ab_window
                            .as_mut()
                            .is_some_and(|(c, ..)| matches!(c.try_wait(), Ok(None)));
                        // Same gate as the audio-only flow: an intentionally
                        // disabled suppression or a missing chain must get
                        // the actionable message, not a window whose device
                        // overlay misdiagnoses it as a missing microphone.
                        // Same transient-churn retry as the ShowWindow probe.
                        let node_present = pipewire::retry_probe(
                            || {
                                pipewire::pw_dump().as_deref().map(|d| {
                                    pipewire::parse_pwdump_nodes(d)
                                        .iter()
                                        .any(|s| s.name == "hushmic_source")
                                })
                            },
                            3,
                            Duration::from_millis(400),
                        );
                        if window_alive {
                            notify::send(
                                Slot::MicTest,
                                "audio-input-microphone",
                                &tr!("notify-mictest-title"),
                                &tr!("notify-window-open-body"),
                            );
                        } else if let Err(blocked) =
                            mictest::precondition(cfg.enabled, node_present, testing)
                        {
                            notify::send(
                                Slot::MicTest,
                                "audio-input-microphone",
                                &tr!("notify-mictest-title"),
                                &blocked.message(),
                            );
                        } else {
                            match spawn_child_window("--test-window", &[]) {
                                Ok(child) => ab_window = Some((child, Instant::now(), true)),
                                Err(e) => {
                                    // No window (headless, exec failure):
                                    // the audio-only flow still works.
                                    eprintln!("hushmic: could not open the test window: {e}");
                                    start_fallback_mictest(
                                        &cfg,
                                        &mut testing,
                                        &mut mictest_cancel,
                                        &tx,
                                    );
                                }
                            }
                        }
                    }
                    TrayCmd::SetupShortcuts => {
                        // First time: the compositor's bind dialog (the
                        // worker reports BindDone, which persists
                        // shortcuts_setup). Once set up, compositors show
                        // the bind dialog only for UNconfigured shortcuts
                        // (KDE re-binds silently, and the portal allows one
                        // bind attempt per session) — so the click becomes
                        // ConfigureShortcuts, the portal's change-keys UI.
                        let cmd = if cfg.shortcuts_setup {
                            shortcuts::Cmd::Configure
                        } else {
                            shortcuts::Cmd::Bind
                        };
                        let _ = shortcuts_cmd.send(cmd);
                    }
                    TrayCmd::About => {
                        // Same PDEATHSIG child pattern as the A/B window; a
                        // failure to open is log-only (nothing to fall back
                        // to, and --about prints its own error on exit).
                        match spawn_child_window("--about", &[]) {
                            Ok(child) => about_windows.push(child),
                            Err(e) => {
                                eprintln!("hushmic: could not open the About window: {e}")
                            }
                        }
                    }
                    TrayCmd::Quit => {
                        let _ = controller.disable();
                        break;
                    }
                }
                if let Err(e) = &applied {
                    eprintln!("hushmic: enable failed: {e}");
                    if gate.on_enable_error(e, true) {
                        notify::send(Slot::Status, "dialog-error", &fail_summary(), e);
                    }
                } else if !cfg.enabled {
                    // user turned it off: stale failure state must not
                    // produce a "running again" notice later
                    gate.reset();
                }
                // A CLI-originated SetMode gets its answer now that the
                // outcome is known: the resulting mode word, or the error.
                if let Some(reply) = control_reply.take() {
                    let msg = match &applied {
                        Ok(()) => control::encode_ok(control::mode_word(
                            cfg.enabled.then(|| controller.mode()),
                        )),
                        Err(e) => control::encode_err(e),
                    };
                    let _ = reply.send(msg);
                }
                if applied.is_ok() && cfg.enabled {
                    // A command may have respawned the chain: heal a stolen
                    // capture stream before the next full tick.
                    schedule_early_tick(&tx);
                }
                let _ = cfg.save();
                // reflect updated state + refreshed mic list + status in the tray
                refresh_tray(
                    &handle,
                    &cfg,
                    &mut controller,
                    &mut known_mics,
                    &mut last_node_present,
                    &mut tray_engine,
                    testing,
                );
            }
            Event::ShowWindow(env) => {
                if !env.is_empty() {
                    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
                    eprintln!(
                        "[hushmic] show request carries display vars: {}",
                        keys.join(" ")
                    );
                }
                // A plain `hushmic` launch — the one that started us, or a
                // second launch forwarded via the show socket: put the A/B
                // window in front of the user. Raising another process's
                // window needs a Wayland activation token we don't have, so
                // an already-open window is closed and respawned — the fresh
                // one appears on top and re-resolves the node pair for free.
                close_ab_window(&mut ab_window);
                // Retried: pw-dump fails transiently under graph churn
                // (which is exactly when windows get opened), and one
                // failed probe must not decline the reopen — root cause
                // of the declined relaunches on the v0.4.0 tag pipelines.
                let node_present = pipewire::retry_probe(
                    || {
                        pipewire::pw_dump().as_deref().map(|d| {
                            pipewire::parse_pwdump_nodes(d)
                                .iter()
                                .any(|s| s.name == "hushmic_source")
                        })
                    },
                    3,
                    Duration::from_millis(400),
                );
                // Same gate as a tray-menu mic test: a disabled chain or a
                // missing node gets the actionable notification, not a
                // window whose device overlay misdiagnoses it.
                if let Err(blocked) = mictest::precondition(cfg.enabled, node_present, testing) {
                    // Journal breadcrumb: without it, a declined reopen is
                    // indistinguishable from a spawn that died instantly.
                    eprintln!("[hushmic] not reopening the A/B window: {blocked:?}");
                    notify::send(
                        Slot::MicTest,
                        "audio-input-microphone",
                        "HushMic",
                        &blocked.message(),
                    );
                } else {
                    match spawn_child_window("--test-window", &env) {
                        // Not user_initiated: never escalate a launch into
                        // the audio-only recording (see ab_window above).
                        Ok(child) => {
                            eprintln!("[hushmic] A/B window opened (pid {})", child.id());
                            ab_window = Some((child, Instant::now(), false));
                        }
                        Err(e) => eprintln!("hushmic: could not open the A/B window: {e}"),
                    }
                }
            }
            Event::MicTestDone(res) => {
                testing = false;
                mictest_cancel = None;
                match res {
                    Ok(()) => {}
                    // Deliberate cancellation (settings changed mid-test)
                    // is information, not an error.
                    Err(e) if e == mictest::CANCELLED_MSG => {
                        notify::send_transient(
                            Slot::MicTest,
                            "audio-input-microphone",
                            &tr!("notify-mictest-title"),
                            &tr!("notify-mictest-cancelled-body"),
                        );
                    }
                    Err(e) => {
                        eprintln!("hushmic: mic test failed: {e}");
                        notify::send(
                            Slot::MicTest,
                            "dialog-error",
                            &tr!("notify-mictest-failed-title"),
                            &e,
                        );
                    }
                }
                let _ = handle.update(move |t: &mut HushMicTray| {
                    t.testing = false;
                });
            }
            Event::Tick => {
                // The chain's engine tier (issue #14) reaches the tray
                // title with up to one tick of lag; no notification.
                tray_engine.push_changed(engine_view(&mut controller), |now| {
                    handle
                        .update(move |t: &mut HushMicTray| {
                            t.engine = now.0;
                            t.engine_light_configured = now.1;
                        })
                        .is_some()
                });
                // Late tray registration: a watcher that appears after
                // login (Cinnamon) or an extension enabled later (GNOME)
                // gets the icon; a desktop without one costs a rare probe.
                // After a minute without a tray, say so once — the
                // microphone is up either way and the CLI controls it.
                if handle.wants_retry() {
                    if tray_backoff.should_attempt() {
                        match traylink::try_spawn(make_tray(&cfg, &known_mics)) {
                            Ok(h) => {
                                eprintln!("hushmic: tray icon registered");
                                handle = h;
                                tray_backoff.record(true);
                                // Everything that happened while Pending.
                                refresh_tray(
                                    &handle,
                                    &cfg,
                                    &mut controller,
                                    &mut known_mics,
                                    &mut last_node_present,
                                    &mut tray_engine,
                                    testing,
                                );
                                let shortcuts_now = shortcuts_up == Some(true);
                                let _ = handle.update(move |t: &mut HushMicTray| {
                                    t.shortcuts_available = shortcuts_now;
                                });
                            }
                            Err(_) => tray_backoff.record(false),
                        }
                    }
                    if !tray_notified && tray_wanted_at.elapsed() >= Duration::from_secs(60) {
                        tray_notified = true;
                        eprintln!(
                            "hushmic: still no system tray after 60 s; running without an icon \
                             (hushmic status | mode | config work from a terminal)"
                        );
                        // Transient: it repeats at every login on a desktop
                        // without a tray and must not pile up.
                        notify::send_transient(
                            Slot::Status,
                            "audio-input-microphone",
                            &tr!("notify-no-tray-title"),
                            &tr!("notify-no-tray-body"),
                        );
                    }
                }
                // Nudge a dead/absent shortcuts portal back to life,
                // through the backoff so a desktop without the portal sees
                // a rare gentle probe, never a hot loop.
                if shortcuts_up == Some(false) && shortcuts_backoff.should_attempt() {
                    let _ = shortcuts_cmd.send(shortcuts::Cmd::Retry);
                }
                // Reap finished About windows (exit status is irrelevant:
                // they are informational, closing one is not a failure to
                // react to). Still-running children are kept for next Tick.
                about_windows.retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_))));
                // Reap the A/B window child. A fast non-zero exit means the
                // window could not start at all (no display / GL) — run the
                // audio-only mic test instead so the click still does
                // something.
                let mut window_quick_fail = None;
                if let Some((child, spawned, user_initiated)) = ab_window.as_mut() {
                    if let Ok(Some(status)) = child.try_wait() {
                        eprintln!(
                            "[hushmic] A/B window exited ({status}) after {:.1}s",
                            spawned.elapsed().as_secs_f32()
                        );
                        // Exit code 1 is --test-window's own "could not
                        // start" path; signals (WM kill) and user closes
                        // (0) must not trigger a surprise audio-only test.
                        // Nor may an auto-opened first-run window: only a
                        // USER-requested test escalates to the audio-only
                        // fallback — anything else records the mic with no
                        // action to answer for it.
                        // The reap window is generous: Tick is 5 s, so the
                        // observed elapsed includes up to a tick of lag.
                        window_quick_fail = Some(
                            status.code() == Some(1)
                                && spawned.elapsed() < Duration::from_secs(15)
                                && *user_initiated,
                        );
                    }
                }
                if let Some(quick_fail) = window_quick_fail {
                    ab_window = None;
                    if quick_fail {
                        notify::send(
                            Slot::MicTest,
                            "audio-input-microphone",
                            &tr!("notify-mictest-title"),
                            &tr!("notify-window-fallback-body"),
                        );
                        start_fallback_mictest(&cfg, &mut testing, &mut mictest_cancel, &tx);
                    }
                }
                // One pw-dump snapshot per tick serves the liveness check, the
                // tray status, a hotplug refresh of the mic list (menu clicks
                // are far too rare to be the only refresh trigger), AND the
                // capture-stream feeder check below.
                let dump = pipewire::pw_dump();
                let nodes = dump.as_deref().map(pipewire::parse_pwdump_nodes);
                let node_present = nodes
                    .as_ref()
                    .map(|v| v.iter().any(|s| s.name == "hushmic_source"));
                last_node_present = node_present;
                if let Some(v) = nodes.as_ref() {
                    let real = pipewire::filter_real(v);
                    if real != known_mics {
                        known_mics = real.clone();
                        let _ = handle.update(move |t: &mut HushMicTray| {
                            t.mics = real;
                        });
                    }
                }

                // Self-heal a re-routed capture stream (issue #5: EasyEffects'
                // "process all input streams" adopts every NEW capture stream
                // onto easyeffects_source, silencing the chain — in the
                // reported setup as a closed hushmic -> EE -> hushmic loop;
                // a stale saved user route breaks the same way). Only OUR
                // effective target is enforced: feeding the chain from
                // easyeffects_source on purpose means pinning it (or having
                // it as default), and then no mismatch arises. No startup
                // grace here: the empty-feeders guard below already keeps
                // the check out of the session manager's initial link setup,
                // and the theft happens AT stream creation — the early tick
                // a spawn schedules heals it before the A/B window's settle
                // overlay wears out its welcome.
                if repin_supported && cfg.enabled && controller.is_running() {
                    if let Some(dump) = dump.as_deref() {
                        let feeders = pipewire::parse_feeders(dump, "hushmic_input");
                        if !feeders.is_empty() {
                            let want = pipewire::repin_want(
                                controller.active_mic(),
                                pipewire::parse_default_source(dump),
                                controller.prior_default(),
                            );
                            if let Some(want) = want {
                                if pipewire::capture_stolen(&feeders, &want) {
                                    repin_streak = repin_streak.saturating_add(1);
                                    // Three corrections in and still stolen:
                                    // something re-asserts its route after
                                    // every one (EasyEffects' process-all-
                                    // inputs). Fighting per tick would flap
                                    // the mic — say so once, retry gently.
                                    if repin_streak == 3 && !repin_notified {
                                        repin_notified = true;
                                        eprintln!(
                                            "[hushmic] another app keeps re-routing \
                                             HushMic's microphone (EasyEffects?) — \
                                             backing off to a slow retry"
                                        );
                                        notify::send(
                                            Slot::Status,
                                            "audio-input-microphone",
                                            &tr!("notify-reroute-summary"),
                                            &tr!("notify-reroute-body"),
                                        );
                                    }
                                    let since = repin_last
                                        .map(|t| t.elapsed().as_secs())
                                        .unwrap_or(u64::MAX);
                                    if pipewire::repin_allowed(repin_streak - 1, since) {
                                        // Both ids come from the same
                                        // snapshot, so they are mutually
                                        // consistent; a respawn racing this
                                        // tick lands the write on a dead id
                                        // and the next tick corrects it.
                                        if let (Some(id), Some(serial)) = (
                                            pipewire::parse_node_id(dump, "hushmic_input"),
                                            pipewire::parse_node_serial(dump, &want),
                                        ) {
                                            eprintln!(
                                                "[hushmic] the capture stream was re-routed \
                                                 to {feeders:?} (another audio tool?) — \
                                                 re-pinning to {want}"
                                            );
                                            repin_last = Some(Instant::now());
                                            if !pipewire::repin_capture(id, serial) {
                                                eprintln!(
                                                    "[hushmic] re-pin failed (pw-metadata \
                                                     error)"
                                                );
                                            }
                                        }
                                    }
                                } else {
                                    repin_streak = 0;
                                }
                            }
                        }
                    }
                }

                // watchdog: if we should be on but the node is gone, re-instantiate.
                //
                // Liveness must be judged by the *node*, not just the child PID:
                // when the PipeWire daemon restarts (or after suspend) the
                // `pipewire -c` child stays alive with a broken connection yet
                // `hushmic_source` disappears, so `is_running()` alone would never
                // fire. `enable()` reaps any lingering child before respawning.
                //
                // "Gone" requires a DEFINITIVE probe (Some(false)): pw-dump
                // failing (None) is a probe error, and tearing down a healthy
                // child over it would be the watchdog causing the very outage
                // it exists to fix. A just-spawned child gets a startup grace.
                //
                // A persistently-broken environment must not respawn every tick
                // and spam the log, so attempts are gated by an exponential
                // backoff (ticks 0,1,3,7,15,31, cap 60) and the "down" line is
                // logged only on the down-state transition.
                let in_grace = controller
                    .secs_since_spawn()
                    .is_some_and(|s| s < STARTUP_GRACE_SECS);
                let down = cfg.enabled
                    && (!controller.is_running() || (node_present == Some(false) && !in_grace));
                if down {
                    if !logged_down {
                        eprintln!("[hushmic] node not running; attempting re-instantiation");
                        logged_down = true;
                    }
                    if backoff.should_attempt() {
                        // The respawn restarts the chain: a mic test recording
                        // it would capture a dead node — cancel it first.
                        if testing {
                            if let Some(c) = &mictest_cancel {
                                c.store(true, Ordering::Relaxed);
                            }
                        }
                        // "Success" must match the liveness model (node present),
                        // polled with a bounded settle window: sampling at t=0
                        // after the spawn records a false failure and escalates
                        // the backoff even though the respawn worked.
                        let ok = match controller.enable(&cfg) {
                            Ok(()) => {
                                let up = controller.is_running()
                                    && pipewire::wait_for_hushmic_source(Duration::from_secs(2));
                                // enable() succeeded yet the node never came:
                                // nothing ever reaches stderr on this path, so
                                // a stuck loop must be surfaced explicitly.
                                if !up && gate.on_silent_failure() {
                                    notify::send(
                                        Slot::Status,
                                        "dialog-error",
                                        &tr!("notify-chain-stuck-summary"),
                                        &tr!("notify-chain-stuck-body"),
                                    );
                                }
                                up
                            }
                            Err(e) => {
                                eprintln!("hushmic: enable failed: {e}");
                                if gate.on_enable_error(&e.to_string(), false) {
                                    notify::send(
                                        Slot::Status,
                                        "dialog-error",
                                        &fail_summary(),
                                        &e.to_string(),
                                    );
                                }
                                false
                            }
                        };
                        backoff.record(ok);
                        if ok {
                            schedule_early_tick(&tx);
                            logged_down = false;
                            // No recovery notice here: right after a respawn
                            // the node is not yet proven STABLE (a flapping
                            // child may die again in seconds). The healthy
                            // branch below emits it once stability holds.
                            // What quick respawn cycles DO prove is
                            // instability — surface that pattern.
                            if gate.on_respawn() {
                                notify::send(
                                    Slot::Status,
                                    "dialog-error",
                                    &tr!("notify-chain-flapping-summary"),
                                    &tr!("notify-chain-flapping-body"),
                                );
                            }
                        }
                    }
                } else if !cfg.enabled || node_present == Some(true) {
                    backoff.record(true); // CONFIRMED healthy -> reset
                    logged_down = false;
                    // Finish a takeover that enable()'s bounded wait missed
                    // (node registered late): no-op unless wanted and pending.
                    if cfg.enabled && node_present == Some(true) {
                        controller.ensure_default_takeover(&cfg, true);
                        if gate.on_healthy() {
                            notify::send(
                                Slot::Status,
                                "audio-input-microphone",
                                &tr!("notify-running-again-summary"),
                                &tr!("notify-running-again-body"),
                            );
                        }
                    } else if !cfg.enabled {
                        gate.reset();
                    }
                }
                // node_present == None with a live child is "unknown", not
                // "healthy": resetting the backoff/log throttle on it would
                // let a flapping pw-dump collapse a fully-escalated backoff
                // to zero and re-log/attempt nearly every tick.

                // --- mic recovery: the watchdog above
                // judges the NODE; this judges the INPUT. When the preferred
                // mic disappears the (healthy) chain restarts onto the system
                // default; when it returns, back onto it — both via the
                // normal enable(), whose resolution renders the right conf
                // either way. config.mic is never touched. All debounce/
                // freeze/cooldown policy lives in watchdog::Recovery.
                if cfg.enabled && !down {
                    let preferred_present = nodes.as_ref().map(|v| {
                        cfg.mic
                            .as_deref()
                            .is_some_and(|name| v.iter().any(|s| s.name == name))
                    });
                    let decision = recovery.observe(
                        cfg.mic.is_some(),
                        controller.active_mic() == cfg.mic.as_deref(),
                        preferred_present,
                        in_grace,
                    );
                    if let Some(switch) = decision {
                        let (log_what, body) = match switch {
                            watchdog::Switch::Fallback => (
                                "preferred microphone disconnected",
                                tr!("notify-mic-fallback-body"),
                            ),
                            watchdog::Switch::Return => (
                                "preferred microphone reconnected",
                                tr!("notify-mic-return-body"),
                            ),
                        };
                        eprintln!("[hushmic] {log_what}; restarting the chain");
                        // Same rule as the watchdog respawn: a running mic
                        // test would record a chain mid-restart.
                        if testing {
                            if let Some(c) = &mictest_cancel {
                                c.store(true, Ordering::Relaxed);
                            }
                        }
                        match controller.enable(&cfg) {
                            Ok(()) => {
                                schedule_early_tick(&tx);
                                notify::send_transient(
                                    Slot::Status,
                                    "audio-input-microphone",
                                    "HushMic",
                                    &body,
                                );
                            }
                            Err(e) => {
                                eprintln!("hushmic: enable failed: {e}");
                                if gate.on_enable_error(&e.to_string(), false) {
                                    notify::send(
                                        Slot::Status,
                                        "dialog-error",
                                        &fail_summary(),
                                        &e.to_string(),
                                    );
                                }
                            }
                        }
                    }
                }

                // reflect liveness in the tray status (icon + title) every tick
                let status = compute_status(&cfg, &mut controller, node_present);
                let testing_now = testing;
                let fallback_now = cfg.enabled
                    && cfg.mic.is_some()
                    && controller.is_running()
                    && controller.active_mic() != cfg.mic.as_deref();
                let _ = handle.update(move |t: &mut HushMicTray| {
                    t.status = status;
                    t.testing = testing_now;
                    t.fallback_active = fallback_now;
                });
            }
            Event::Shutdown => {
                // SIGTERM/SIGINT/SIGHUP: restore the default mic + reap the
                // child, then leave the loop.
                let _ = controller.disable();
                break;
            }
            // consumed by the preprocessing above (answered inline or
            // rewritten into a synthetic SetMode command)
            Event::Control(_) => unreachable!("control requests are preprocessed"),
            Event::Shortcut(_) => unreachable!("shortcut events are preprocessed"),
        }
    }
    // Orderly shutdown removes the control socket; a crash leaves it for
    // the next start's unlink+rebind (and clients' connects fail = exit 2).
    let _ = std::fs::remove_file(&control_socket_path);
    // Quit/Shutdown may interrupt a running mic test: its worker dies with
    // the process (recorders via PDEATHSIG) before its own cleanup runs —
    // the voice recordings must not outlive the app. (Unlinking files the
    // recorders still hold open is fine: the data dies with their fds.)
    mictest::remove_recordings();
}

#[cfg(test)]
mod tests {
    use super::{EngineCache, EngineView};
    use hushmic::diagnostics::EngineTier;

    /// A tray that is not there yet (TrayLink::Pending) drops the update.
    /// The cache must not record it as shown, or the tier the tray finally
    /// registers with stays wrong until it happens to change again.
    #[test]
    fn a_dropped_engine_update_is_pushed_again() {
        let mut cache = EngineCache::default();
        let mut sent: Vec<EngineView> = Vec::new();
        let light: EngineView = (Some(EngineTier::Light), false);
        // No tray: the push is attempted and does not land.
        cache.push_changed(light, |v| {
            sent.push(v);
            false
        });
        assert_eq!(sent.len(), 1);
        // Same value, still no tray: attempted again, not assumed shown.
        cache.push_changed(light, |v| {
            sent.push(v);
            false
        });
        assert_eq!(sent.len(), 2);
        // The tray registers and takes it.
        cache.push_changed(light, |v| {
            sent.push(v);
            true
        });
        assert_eq!(sent.len(), 3);
        // Now it is known to be shown, so nothing is resent.
        cache.push_changed(light, |_| panic!("resent a value the tray shows"));
        assert_eq!(sent.len(), 3);
    }

    /// The displayed state is the pair, not the tier alone: the same
    /// `light` tier reads as a fallback or as the configured model
    /// depending on the second field, and a change there must reach the
    /// tray (a per-mic profile switch does exactly this).
    #[test]
    fn the_configured_light_flag_is_part_of_the_displayed_state() {
        let mut cache = EngineCache::default();
        let mut sent: Vec<EngineView> = Vec::new();
        let fallback: EngineView = (Some(EngineTier::Light), false);
        let configured: EngineView = (Some(EngineTier::Light), true);
        cache.push_changed(fallback, |v| {
            sent.push(v);
            true
        });
        cache.push_changed(configured, |v| {
            sent.push(v);
            true
        });
        assert_eq!(sent, vec![fallback, configured]);
    }

    /// A full tray refresh carries the engine fields, so it also decides
    /// what the cache holds: landed means shown, dropped means unknown.
    #[test]
    fn a_full_refresh_sets_the_cache_and_a_dropped_one_clears_it() {
        let mut cache = EngineCache::default();
        let quality: EngineView = (Some(EngineTier::Quality), false);
        cache.record(quality, true);
        cache.push_changed(quality, |_| panic!("resent what the refresh sent"));
        // A refresh that reached no tray leaves the state unknown.
        cache.record(quality, false);
        let mut sent = 0;
        cache.push_changed(quality, |_| {
            sent += 1;
            true
        });
        assert_eq!(sent, 1);
    }
}
