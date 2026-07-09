use hushmic::config::Config;
use hushmic::controller::{self, Controller, Paths};
use hushmic::pipewire;
use hushmic::tray::{HushMicTray, TrayCmd, TrayStatus};
use hushmic::{autostart, lock, watchdog};
use ksni::blocking::TrayMethods;
use std::io::Read;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// Unifies the event sources (tray commands, watchdog ticks, termination
/// signals) into one channel so the main loop is single-threaded and owns the
/// `Controller`.
enum Event {
    Cmd(TrayCmd),
    Tick,
    Shutdown,
}

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
        TrayStatus::Active
    } else {
        TrayStatus::Error
    }
}

/// Acquire the single-instance lock, or exit if another hushmic already holds
/// it (a second tray + filter-chain would fight over `hushmic_source`).
fn acquire_single_instance() -> std::fs::File {
    match lock::try_lock(&lock::default_lock_path()) {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("hushmic is already running.");
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tray_mode = args.iter().any(|a| a == "--tray");
    let enable_once = args.iter().any(|a| a == "--enable-once");
    let unrecognized = args.iter().any(|a| a != "--tray" && a != "--enable-once");
    if unrecognized || (tray_mode && enable_once) || (!tray_mode && !enable_once) {
        eprintln!("usage: hushmic --tray          run the system-tray app");
        eprintln!("       hushmic --enable-once   headless: enable the mic until terminated");
        std::process::exit(2);
    }

    let _lock = acquire_single_instance();
    let shutdown_pipe = install_signal_handlers();

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
        eprintln!("hushmic: enabled; send SIGTERM or press Ctrl+C to stop.");
        wait_for_shutdown(shutdown_pipe);
        drop(c);
        return;
    }

    let mut cfg = Config::load();
    let mut controller = Controller::new(Paths::resolve());

    let (tx, rx) = mpsc::channel::<Event>();

    // Tray -> commands
    let (ctx, crx) = mpsc::channel::<TrayCmd>();
    let mut known_mics = pipewire::list_real_sources();
    let tray = HushMicTray {
        cfg: cfg.clone(),
        mics: known_mics.clone(),
        cmd_tx: ctx,
        status: TrayStatus::Off,
    };
    let handle = match tray.spawn() {
        Ok(h) => h,
        Err(e) => {
            eprintln!(
                "hushmic: could not register a system tray icon ({e}). On GNOME, install the \
                 'AppIndicator and KStatusNotifierItem Support' extension; KDE and most other \
                 desktops provide it out of the box."
            );
            std::process::exit(1);
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

    // apply persisted state on launch
    if cfg.autostart != autostart::is_autostart_enabled() {
        let _ = autostart::set_autostart(cfg.autostart);
    }
    if cfg.enabled {
        if let Err(e) = controller.enable(&cfg) {
            eprintln!("hushmic: enable failed: {e}");
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

    let apply = |controller: &mut Controller, cfg: &Config| {
        if cfg.enabled {
            if let Err(e) = controller.enable(cfg) {
                eprintln!("hushmic: enable failed: {e}");
            }
        } else if let Err(e) = controller.disable() {
            eprintln!("hushmic: disable failed: {e}");
        }
    };

    // watchdog respawn backoff + throttled "node down" logging (state-change only)
    let mut backoff = hushmic::watchdog::Backoff::new();
    let mut logged_down = false;

    for ev in rx {
        match ev {
            Event::Cmd(cmd) => {
                match cmd {
                    TrayCmd::SetEnabled(v) => {
                        cfg.enabled = v;
                        apply(&mut controller, &cfg);
                    }
                    TrayCmd::SelectMic(m) => {
                        cfg.mic = m;
                        if cfg.enabled {
                            apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SelectModel(m) => {
                        cfg.model = m;
                        if cfg.enabled {
                            apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SetAttn(v) => {
                        cfg.attn_limit = v;
                        if cfg.enabled {
                            apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SetDefaultToggle(v) => {
                        cfg.set_default = v;
                        if cfg.enabled {
                            apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SetAutostart(v) => {
                        cfg.autostart = v;
                        let _ = autostart::set_autostart(v);
                    }
                    TrayCmd::Quit => {
                        let _ = controller.disable();
                        break;
                    }
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
                let status = compute_status(&cfg, &mut controller, node_present);
                let new_mics = known_mics.clone();
                let snapshot = cfg.clone();
                let _ = handle.update(move |t: &mut HushMicTray| {
                    t.cfg = snapshot;
                    t.mics = new_mics;
                    t.status = status;
                });
            }
            Event::Tick => {
                // One pw-dump snapshot per tick serves the liveness check, the
                // tray status, AND a hotplug refresh of the mic list (menu
                // clicks are far too rare to be the only refresh trigger).
                let nodes = pipewire::sources_snapshot();
                let node_present = nodes
                    .as_ref()
                    .map(|v| v.iter().any(|s| s.name == "hushmic_source"));
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
                        // "Success" must match the liveness model (node present),
                        // polled with a bounded settle window: sampling at t=0
                        // after the spawn records a false failure and escalates
                        // the backoff even though the respawn worked.
                        let ok = match controller.enable(&cfg) {
                            Ok(()) => {
                                controller.is_running()
                                    && pipewire::wait_for_hushmic_source(Duration::from_secs(2))
                            }
                            Err(e) => {
                                eprintln!("hushmic: enable failed: {e}");
                                false
                            }
                        };
                        backoff.record(ok);
                        if ok {
                            logged_down = false;
                        }
                    }
                } else if !cfg.enabled || node_present == Some(true) {
                    backoff.record(true); // CONFIRMED healthy -> reset
                    logged_down = false;
                    // Finish a takeover that enable()'s bounded wait missed
                    // (node registered late): no-op unless wanted and pending.
                    if cfg.enabled && node_present == Some(true) {
                        controller.ensure_default_takeover(&cfg, true);
                    }
                }
                // node_present == None with a live child is "unknown", not
                // "healthy": resetting the backoff/log throttle on it would
                // let a flapping pw-dump collapse a fully-escalated backoff
                // to zero and re-log/attempt nearly every tick.
                // reflect liveness in the tray status (icon + title) every tick
                let status = compute_status(&cfg, &mut controller, node_present);
                let _ = handle.update(move |t: &mut HushMicTray| {
                    t.status = status;
                });
            }
            Event::Shutdown => {
                // SIGTERM/SIGINT/SIGHUP: restore the default mic + reap the
                // child, then leave the loop.
                let _ = controller.disable();
                break;
            }
        }
    }
}
