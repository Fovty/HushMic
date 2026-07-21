use hushmic::config::Config;
use hushmic::control;
use hushmic::controller::{self, Controller, Paths, RunMode};
use hushmic::notify::{self, FailureGate, Slot};
use hushmic::pipewire;
use hushmic::tray::{HushMicTray, TrayCmd, TrayStatus};
use hushmic::{autostart, lock, mictest, watchdog};
use ksni::blocking::TrayMethods;
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
    /// instance already running and forwards itself via the show socket.
    ShowWindow,
    /// A CLI request from the control socket; the reply goes back through
    /// the embedded sender once the outcome is known.
    Control(control::ControlReq),
    Shutdown,
}

/// Notification text for enable/watchdog failures (the body carries the
/// actionable enable() error).
const FAIL_SUMMARY: &str = "HushMic could not start the virtual microphone";
const STUCK_SUMMARY: &str = "HushMic keeps losing the virtual microphone";
const STUCK_BODY: &str = "Re-creating it keeps failing — is PipeWire running? \
                          Run `hushmic --tray` from a terminal for details.";
const FLAP_SUMMARY: &str = "HushMic keeps restarting the virtual microphone";
const FLAP_BODY: &str = "It went down and was re-created several times in the last few \
                         minutes — the audio setup may be unstable. Run `hushmic --tray` \
                         from a terminal for details.";

/// How long a freshly spawned filter-chain host gets to register its node
/// before an absent `hushmic_source` counts as "down". Registration normally
/// takes well under a second; without the grace, the status/watchdog sampled
/// right after a spawn would flash Error / respawn a healthy child.
const STARTUP_GRACE_SECS: u64 = 3;

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
                eprintln!("hushmic is already running.");
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
fn spawn_child_window(mode_flag: &str) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let mut c = std::process::Command::new(exe);
    c.arg(mode_flag);
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
        .map_err(String::from)
        .and_then(|()| {
            let traced = dump
                .as_deref()
                .and_then(|d| mictest::find_feeding_node(d, "hushmic_input"));
            mictest::raw_target(traced, cfg.mic.as_deref())
                .ok_or_else(|| "Could not find the microphone feeding HushMic.".to_string())
        });
    match start {
        Err(msg) => notify::send(Slot::MicTest, "audio-input-microphone", "Mic test", &msg),
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Control subcommands are CLIENT invocations: talk to the running
    // tray's socket, print, exit. Same SIGPIPE stance as --version.
    if matches!(
        args.first().map(|s| s.as_str()),
        Some("status" | "mode" | "toggle")
    ) {
        use std::io::Write;
        let (code, out) = control::client_run(&args);
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
    let enable_once = args.iter().any(|a| a == "--enable-once");
    let test_window = args.iter().any(|a| a == "--test-window");
    let about = args.iter().any(|a| a == "--about");
    let version = args.iter().any(|a| a == "--version");
    let doctor = args.iter().any(|a| a == "--doctor");
    const KNOWN_FLAGS: [&str; 6] = [
        "--tray",
        "--enable-once",
        "--test-window",
        "--about",
        "--version",
        "--doctor",
    ];
    let unrecognized = args.iter().any(|a| !KNOWN_FLAGS.contains(&a.as_str()));
    let modes = [tray_mode, enable_once, test_window, about, version, doctor]
        .iter()
        .filter(|m| **m)
        .count();
    if unrecognized || modes > 1 {
        eprintln!("usage: hushmic                 start the tray and open the A/B window");
        eprintln!("                               (an already-running instance opens it instead)");
        eprintln!("       hushmic --tray          run the system-tray app (no window; autostart)");
        eprintln!("       hushmic --enable-once   headless: enable the mic until terminated");
        eprintln!("       hushmic --test-window   open the live A/B mic-test window");
        eprintln!("       hushmic --about         open the About window");
        eprintln!("       hushmic --version       print the version and install paths");
        eprintln!(
            "       hushmic --doctor        print a diagnostics report (exits 1 on problems)"
        );
        eprintln!("       hushmic status [--json] show what the running tray is doing");
        eprintln!("       hushmic mode [STATE]    print or set: suppress|bypass|mute|off");
        eprintln!("       hushmic toggle mute|bypass   hotkey-friendly overlay toggle");
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
        // pw-cat before the mid-2022 rework (Ubuntu 22.04 ships 0.3.48) cannot
        // stream a capture to a pipe — which is how the live view reads audio —
        // so the A/B window can only sit at −∞ there. Explain and exit 1: the
        // tray then runs the file-based recording test (pw-cat writes a real
        // file fine on every version), reusing the same path as the no-GL
        // fallback. Standalone `--test-window` just prints the reason and exits.
        if !pipewire::supports_pipe_capture() {
            let reason = "The live A/B view needs a newer PipeWire on this system.";
            eprintln!("hushmic: {reason}");
            // Bounded wait, not fire-and-forget: the detached send worker dies
            // with the process on the exit below and the notification would be
            // lost (same reason main()'s could-not-start path uses this).
            notify::send_and_wait(
                Slot::MicTest,
                "audio-input-microphone",
                "Mic test",
                reason,
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
    let mut controller = Controller::new(Paths::resolve());

    // A previous run that died mid-mic-test never got to delete its
    // recordings — the user's voice must not linger on disk.
    mictest::remove_recordings();

    let (tx, rx) = mpsc::channel::<Event>();

    // Tray -> commands
    let (ctx, crx) = mpsc::channel::<TrayCmd>();
    let mut known_mics = pipewire::list_real_sources();
    // At login we can outrun the desktop's StatusNotifierWatcher: Cinnamon's
    // xapp-sn-watcher only registers once the applets load and is not
    // DBus-activatable, so an autostarted instance raced it, failed to
    // register, and exited — indistinguishable from "autostart is broken"
    // (reproduced 3x on Mint 22.1). The watcher follows within seconds on
    // every desktop we bench, so retry over a bounded window before
    // declaring the environment tray-less.
    const TRAY_WAIT_SECS: u64 = 60;
    let spawn_deadline = std::time::Instant::now() + Duration::from_secs(TRAY_WAIT_SECS);
    let mut reported_wait = false;
    let handle = loop {
        let tray = HushMicTray {
            cfg: cfg.clone(),
            mics: known_mics.clone(),
            cmd_tx: ctx.clone(),
            status: TrayStatus::Off,
            testing: false,
            fallback_active: false,
            mode: RunMode::default(),
        };
        // Inside a Flatpak the session-bus proxy only lets us own names under
        // our app ID, so registering the spec's well-known
        // `org.kde.StatusNotifierItem-{pid}-{id}` name is denied and a plain
        // spawn() fails outright. ksni's sanctioned fallback registers by
        // unique connection name only (same solution as Chromium's).
        let spawned = if hushmic::sandbox::is_flatpak() {
            tray.disable_dbus_name(true).spawn()
        } else {
            tray.spawn()
        };
        match spawned {
            Ok(h) => break h,
            Err(e) if std::time::Instant::now() < spawn_deadline => {
                if !reported_wait {
                    eprintln!(
                        "hushmic: no system tray yet ({e}); waiting up to {TRAY_WAIT_SECS}s \
                         for one to appear…"
                    );
                    reported_wait = true;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(e) => {
                let msg = format!(
                    "Could not register a system tray icon ({e}). On GNOME, install the \
                     'AppIndicator and KStatusNotifierItem Support' extension; KDE and most other \
                     desktops provide it out of the box."
                );
                eprintln!("hushmic: {msg}");
                // The one failure a tray app cannot show in the tray — and, on
                // stock GNOME, exactly the case where notifications still work.
                // Bounded wait: the process exits right after, and a detached
                // send thread would be killed mid-call.
                notify::send_and_wait(
                    Slot::Status,
                    "dialog-error",
                    "HushMic could not start",
                    &msg,
                    Duration::from_secs(2),
                );
                std::process::exit(1);
            }
        }
    };

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
                if conn.is_err() || tx.send(Event::ShowWindow).is_err() {
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

    // Gate for failure/recovery notifications: dedups the watchdog's
    // backoff-gated retries so the same error does not re-pop forever.
    let mut gate = FailureGate::new();

    // apply persisted state on launch
    let _ = autostart::reconcile(cfg.autostart);
    if cfg.enabled {
        if let Err(e) = controller.enable(&cfg) {
            eprintln!("hushmic: enable failed: {e}");
            // A launch failure is invisible on stderr for .desktop/autostart
            // starts — surface it (launch counts as user-initiated).
            if gate.on_enable_error(&e.to_string(), true) {
                notify::send(Slot::Status, "dialog-error", FAIL_SUMMARY, &e.to_string());
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
        let _ = tx.send(Event::ShowWindow);
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
                        };
                        let payload = if json {
                            control::render_status_json(&s)
                        } else {
                            control::render_status_human(&s)
                        };
                        let _ = req.reply.send(control::encode_ok(&payload));
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
                                "Mic test",
                                "The A/B test window is already open.",
                            );
                        } else if let Err(msg) =
                            mictest::precondition(cfg.enabled, node_present, testing)
                        {
                            notify::send(Slot::MicTest, "audio-input-microphone", "Mic test", msg);
                        } else {
                            match spawn_child_window("--test-window") {
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
                    TrayCmd::About => {
                        // Same PDEATHSIG child pattern as the A/B window; a
                        // failure to open is log-only (nothing to fall back
                        // to, and --about prints its own error on exit).
                        match spawn_child_window("--about") {
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
                        notify::send(Slot::Status, "dialog-error", FAIL_SUMMARY, e);
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
                let _ = cfg.save();
                // reflect updated state + refreshed mic list + status in the tray
                let nodes = pipewire::sources_snapshot();
                let node_present = nodes
                    .as_ref()
                    .map(|v| v.iter().any(|s| s.name == "hushmic_source"));
                if let Some(v) = nodes {
                    known_mics = pipewire::filter_real(&v);
                }
                last_node_present = node_present;
                let status = compute_status(&cfg, &mut controller, node_present);
                let new_mics = known_mics.clone();
                let snapshot = cfg.clone();
                let testing_now = testing;
                let fallback_now = cfg.enabled
                    && cfg.mic.is_some()
                    && controller.is_running()
                    && controller.active_mic() != cfg.mic.as_deref();
                let mode_now = controller.mode();
                let _ = handle.update(move |t: &mut HushMicTray| {
                    t.cfg = snapshot;
                    t.mics = new_mics;
                    t.status = status;
                    t.testing = testing_now;
                    t.fallback_active = fallback_now;
                    t.mode = mode_now;
                });
            }
            Event::ShowWindow => {
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
                if let Err(msg) = mictest::precondition(cfg.enabled, node_present, testing) {
                    // Journal breadcrumb: without it, a declined reopen is
                    // indistinguishable from a spawn that died instantly.
                    eprintln!("[hushmic] not reopening the A/B window: {msg}");
                    notify::send(Slot::MicTest, "audio-input-microphone", "HushMic", msg);
                } else {
                    match spawn_child_window("--test-window") {
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
                            "Mic test",
                            &e,
                        );
                    }
                    Err(e) => {
                        eprintln!("hushmic: mic test failed: {e}");
                        notify::send(Slot::MicTest, "dialog-error", "Mic test failed", &e);
                    }
                }
                let _ = handle.update(move |t: &mut HushMicTray| {
                    t.testing = false;
                });
            }
            Event::Tick => {
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
                            "Mic test",
                            "The test window could not start — running the audio-only \
                             mic test instead.",
                        );
                        start_fallback_mictest(&cfg, &mut testing, &mut mictest_cancel, &tx);
                    }
                }
                // One pw-dump snapshot per tick serves the liveness check, the
                // tray status, AND a hotplug refresh of the mic list (menu
                // clicks are far too rare to be the only refresh trigger).
                let nodes = pipewire::sources_snapshot();
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
                                        STUCK_SUMMARY,
                                        STUCK_BODY,
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
                                        FAIL_SUMMARY,
                                        &e.to_string(),
                                    );
                                }
                                false
                            }
                        };
                        backoff.record(ok);
                        if ok {
                            logged_down = false;
                            // No recovery notice here: right after a respawn
                            // the node is not yet proven STABLE (a flapping
                            // child may die again in seconds). The healthy
                            // branch below emits it once stability holds.
                            // What quick respawn cycles DO prove is
                            // instability — surface that pattern.
                            if gate.on_respawn() {
                                notify::send(Slot::Status, "dialog-error", FLAP_SUMMARY, FLAP_BODY);
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
                                "HushMic is running again",
                                "The virtual microphone is back up.",
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
                                "Your microphone was disconnected — HushMic is \
                                 following the system default for now.",
                            ),
                            watchdog::Switch::Return => (
                                "preferred microphone reconnected",
                                "Your microphone is back — HushMic switched back \
                                 to it.",
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
                            Ok(()) => notify::send_transient(
                                Slot::Status,
                                "audio-input-microphone",
                                "HushMic",
                                body,
                            ),
                            Err(e) => {
                                eprintln!("hushmic: enable failed: {e}");
                                if gate.on_enable_error(&e.to_string(), false) {
                                    notify::send(
                                        Slot::Status,
                                        "dialog-error",
                                        FAIL_SUMMARY,
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
