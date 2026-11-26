use hushmic::appstate::{AppRef, DirectUse, ShortcutsBackend, ShortcutsState, TrayHost};
use hushmic::config::Config;
use hushmic::control;
use hushmic::controller::{self, Controller, Paths, RunMode};
use hushmic::notify::{self, FailureGate, Slot};
use hushmic::pipewire;
use hushmic::portwriter;
use hushmic::snapshot::{self, Lifecycle, Start};
use hushmic::tr;
use hushmic::tray::{HushMicTray, TrayCmd, TrayExtras, TrayStatus};
use hushmic::traylink::{self, TrayLink};
use hushmic::{apps, autostart, lock, mictest, shortcuts, watch, watchdog, windowstate};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Unifies the event sources (tray commands, watchdog ticks, mic-test
/// completion, termination signals) into one channel so the main loop is
/// single-threaded and owns the `Controller`.
enum Event {
    Cmd(TrayCmd),
    Tick,
    MicTestDone(Result<(), String>),
    /// Put the window in front of the user: sent whenever a second
    /// (flag-less) launch finds this instance already running and forwards
    /// itself via the show socket, carrying the caller's display variables
    /// (a `systemd --user` daemon has none, and the window must open on the
    /// caller's screen).
    ShowWindow(Vec<(String, String)>),
    /// A setting the window changed was applied but could not be written
    /// (the config writer's line).
    Unwritten(String),
    /// A CLI request from the control socket; the reply goes back through
    /// the embedded sender once the outcome is known.
    Control(control::ControlReq),
    /// A report from the global-shortcuts portal worker: availability,
    /// key events, or a finished bind dialog.
    Shortcut(shortcuts::PortalEvent),
    /// Something the window watches changed outside the loop's own events
    /// (the engine tier): the loop publishes a fresh snapshot.
    Wake,
    /// A link job ended: the chain of that generation is linked into the
    /// source host (or why not).
    Linked(hushmic::link::Done),
    /// The desktop's style or reduced-motion wish, first read or changed.
    Desktop(hushmic::desktop::Look),
    /// The session the closed window was in has been watched: whether it
    /// stays (not a logout).
    CloseNoticeDue(bool),
    /// The window with this pid had its [`SETTLE_GRACE`].
    WindowSettled(u32),
    /// A graph probe came back (see `start_graph_probe`): the pw-dump (None:
    /// it failed), the chain generation when it started, and whether the
    /// default takeover had landed by then.
    Graph {
        dump: Option<String>,
        generation: u64,
        /// The speakers chain's generation when it was asked for.
        #[cfg(feature = "speakers")]
        speakers_generation: u64,
        landed_before: bool,
    },
    /// A live write of the chain's Mode or Attenuation port did not land
    /// (the chain generation it was for); see `Controller::write_mode_live`.
    PortWriteFailed(u64),
    /// A default-source takeover attempt finished on its worker; see
    /// `Controller::takeover_finished`.
    TakeoverDone,
    /// A file worker's word (see `files::jobs`).
    Files(hushmic::files::jobs::JobEvent),
    /// "Save all" on the note that files are ready.
    FilesSaveAll,
    /// The engine began a run (`usemark`): someone records HushMic now,
    /// maybe an app the last snapshot did not show yet.
    RunBegan,
    /// The capture healer owes a look by then (`pipewire::Repin::look_due`).
    HealerLook(Instant),
    /// A device or stream came or went (`pipewire::watch_graph`), or a
    /// pending microphone switch is due: the graph is read now, outside
    /// the tick's count (UX panel round 2, A5).
    Look,
    Shutdown,
}

/// What the file workers run: the Settings' model and inference, and
/// what `base` found at startup (ffmpeg, and a test's fake workers).
fn files_engine(
    cfg: &Config,
    base: &hushmic::files::jobs::EngineChoice,
) -> hushmic::files::jobs::EngineChoice {
    hushmic::files::jobs::EngineChoice {
        model: cfg.model.clone(),
        inference: match cfg.inference {
            hushmic::config::Inference::Auto => hushmic_denoiser::Inference::Auto,
            hushmic::config::Inference::Onnx => hushmic_denoiser::Inference::Onnx,
        },
        ..base.clone()
    }
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

/// The budget for `hushmic status` to re-read an idle chain's state on the
/// main loop. pw-dump answers in tens of ms; past this the reading is
/// unknown and the status line says plain `running`.
const STATUS_PROBE_BUDGET: Duration = Duration::from_millis(500);

/// A `refresh` within this long of the last graph read answers from it.
const REFRESH_FRESH: Duration = Duration::from_secs(1);

/// One extra Tick shortly after a chain spawn. The 5 s watchdog cadence is
/// the wrong clock for healing a capture stream that another audio tool
/// re-routed at creation (issue #5: EasyEffects) — without this the window
/// waits on a mic that is not fed for most of a tick.
fn schedule_early_tick(tx: &mpsc::Sender<Event>) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let _ = tx.send(Event::Tick);
    });
}

/// A graph probe out this long means the session manager is not answering
/// (said once in the log).
const GRAPH_SLOW: Duration = Duration::from_secs(10);
/// A graph probe that has not answered by then lands as a failed read:
/// it counts toward "PipeWire unreachable", and the tick's other checks
/// (a dead chain host) still run.
const GRAPH_BUDGET: Duration = Duration::from_secs(15);
/// A new run of the engine asks for a graph probe at most this often.
const RUN_PROBE_EVERY: Duration = Duration::from_secs(1);

/// Read the graph off the loop: pw-dump waits for the session manager, and
/// while WirePlumber is silent it would wait forever. One probe at a time,
/// bounded by [`GRAPH_BUDGET`]; its result comes back as [`Event::Graph`].
fn start_graph_probe(tx: &mpsc::Sender<Event>, in_flight: &mut Option<Instant>, landed: bool) {
    if in_flight.is_some() {
        return;
    }
    *in_flight = Some(Instant::now());
    let generation = hushmic::diagnostics::engine_generation();
    #[cfg(feature = "speakers")]
    let speakers_generation =
        hushmic::diagnostics::engine_generation_of(hushmic::diagnostics::Chain::Speakers);
    let tx = tx.clone();
    std::thread::spawn(move || {
        let dump = pipewire::pw_dump_within(GRAPH_BUDGET);
        let _ = tx.send(Event::Graph {
            dump,
            generation,
            #[cfg(feature = "speakers")]
            speakers_generation,
            landed_before: landed,
        });
    });
}

fn tray_host(handle: &TrayLink) -> TrayHost {
    if handle.is_sni() {
        TrayHost::Registered
    } else if handle.wants_retry() {
        TrayHost::Waiting
    } else {
        TrayHost::Disabled
    }
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
fn acquire_single_instance(forward_show: bool, mode: &str) -> std::fs::File {
    let path = lock::default_lock_path();
    match lock::try_lock(&path) {
        Ok(Some(f)) => {
            // the unit's own start says so: a second start then names the
            // two login starts as the likely cause
            if lock::Invocation::here().by_systemd() {
                lock::record_holder(&f, &format!("{mode}{}", lock::HOLDER_UNIT));
            } else {
                lock::record_holder(&f, mode);
            }
            f
        }
        Ok(None) => {
            if forward_show && lock::request_show(&lock::default_show_socket_path()) {
                // what the click asked for: a window
                eprintln!("hushmic is already running; asked it to open its window.");
                std::process::exit(0);
            }
            // Not 0 for a terminal or a script: nothing was started. Under
            // systemd 0, as before: a unit generated by an earlier version
            // has no RestartPreventExitStatus, and Restart=on-failure
            // leaves a clean exit alone.
            let under_systemd = lock::Invocation::here().by_systemd();
            let (code, line) = lock::already_running(under_systemd, lock::holder(&path));
            eprintln!("{line}");
            std::process::exit(code);
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

/// Spawn the window as a child of the daemon (same binary, `--window`).
/// PDEATHSIG binds it to the daemon: without it there is no virtual mic,
/// and an orphaned window would only show a dead state.
/// MUST be called from the main thread (PR_SET_PDEATHSIG is thread-scoped).
/// `env` overlays display variables from a forwarded show request.
fn spawn_child_window(
    args: &[&str],
    env: &[(String, String)],
) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    // The binary that runs, also after an upgrade replaced its file.
    let mut c = hushmic::selfexe::command();
    c.args(args);
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

/// What a chain restart invalidates: a running `hushmic miccheck` (it would
/// record a dying or reborn node). The window's Mic check notices a restart
/// by itself (a new engine meter). Idempotent.
fn invalidate_mic_consumers(testing: bool, mictest_cancel: &Option<Arc<AtomicBool>>) {
    if testing {
        if let Some(c) = mictest_cancel {
            c.store(true, Ordering::Relaxed);
        }
    }
}

/// Bring a running (enabled) chain to `cfg` with the smallest change
/// (`Controller::apply_settings`: nothing, a live strength write, or a
/// restart), invalidating the mic test exactly when the chain restarts:
/// ahead of a planned restart, and after the rare live-write fallback.
/// Errors as the enable message so the caller can notify.
fn apply_mic(
    controller: &mut Controller,
    cfg: &Config,
    testing: bool,
    mictest_cancel: &Option<Arc<AtomicBool>>,
) -> Result<(), String> {
    let planned = controller.delta(cfg).restarts();
    if planned {
        invalidate_mic_consumers(testing, mictest_cancel);
    }
    match controller.apply_settings(cfg) {
        Ok(controller::Applied::Restarted) if !planned => {
            invalidate_mic_consumers(testing, mictest_cancel);
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// The v1 companion window child (PDEATHSIG-bound to the daemon). One at
/// a time; chain changes never close it (its pages follow the snapshot).
struct Companion {
    child: std::process::Child,
    spawned: Instant,
    /// The window has keyboard focus (`ui focused on`); a new one starts
    /// without until it says so.
    focused: bool,
    /// The page it shows (`--page`, then `ui page`).
    page: hushmic::appstate::Page,
    /// New, and it has neither reported focus nor had [`SETTLE_GRACE`]:
    /// it counts as showing its page (a launch's failure must not beat
    /// the window it is about to show in; a relaunch's window takes over
    /// what was held for the old one).
    settling: bool,
    /// The logind session it opened in (looked up off the loop): the one
    /// watched for a logout after its close.
    session: Arc<OnceLock<Option<String>>>,
    /// What Live's Status card shows (`ui shows`).
    live: notify::LiveShows,
}

/// Windows the daemon replaced or reaped: a late word from one of them is
/// not about the window open now.
fn retired_windows() -> &'static Mutex<VecDeque<u32>> {
    static RETIRED: OnceLock<Mutex<VecDeque<u32>>> = OnceLock::new();
    RETIRED.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn retire_window(pid: u32) {
    if let Ok(mut r) = retired_windows().lock() {
        if r.len() == 8 {
            r.pop_front();
        }
        r.push_back(pid);
    }
}

/// A new window got a pid a retired one had.
fn unretire_window(pid: u32) {
    if let Ok(mut r) = retired_windows().lock() {
        r.retain(|p| *p != pid);
    }
}

fn window_retired(pid: u32) -> bool {
    retired_windows().lock().is_ok_and(|r| r.contains(&pid))
}

/// How long a new window may take to report focus before it no longer
/// counts as showing its page.
const SETTLE_GRACE: Duration = Duration::from_secs(2);

/// After On, how long the streams Off moved wait before they are moved
/// back: the session manager moves the ones that follow the default by
/// itself once HushMic is the default again (UX panel A1).
const RETURN_AFTER: Duration = Duration::from_secs(2);
/// After On, how long HushMic's node may take to appear before the moved
/// streams are left where they are.
const RETURN_GIVE_UP: Duration = Duration::from_secs(60);

/// The problems a window shows: its page's, while it has focus or is still
/// settling ([`Companion::settling`]); none otherwise.
fn window_mask(
    focused: bool,
    settling: bool,
    page: hushmic::appstate::Page,
    live: notify::LiveShows,
) -> u16 {
    if focused || settling {
        notify::shown_on(page, live)
    } else {
        0
    }
}

/// The window shows Live to the user (with focus, or settling): a problem
/// its Status card could show waits for the card's word
/// ([`notify::CARD_GRACE`]).
fn on_live(focused: bool, settling: bool, page: hushmic::appstate::Page) -> bool {
    (focused || settling) && page == hushmic::appstate::Page::Live
}

/// A problem the window shows is held back instead of also becoming a
/// desktop notification; once it no longer does, the held ones go out
/// ([`release_held`]).
fn sync_window_shows(win: &Option<Companion>) {
    notify::set_window_shows(
        win.as_ref()
            .map_or(0, |c| window_mask(c.focused, c.settling, c.page, c.live)),
    );
    notify::set_window_on_live(
        win.as_ref()
            .is_some_and(|c| on_live(c.focused, c.settling, c.page)),
    );
}

/// The phase the window was last shown, and the recovery count when the
/// chain last stopped being ready (the outage's start).
#[derive(Default)]
struct Shown {
    phase: hushmic::appstate::Phase,
    outage_base: u64,
}

impl Shown {
    /// The next snapshot's phase, with the failure gate's recovery count.
    /// True when the window showed the chain reconnecting and it is ready
    /// again, with nothing counted for this outage yet (a failure's
    /// recovery counts on its own, [`FailureGate::on_up`]).
    fn back_after_restart(&mut self, now: hushmic::appstate::Phase, recoveries: u64) -> bool {
        use hushmic::appstate::Phase;
        let before = std::mem::replace(&mut self.phase, now);
        if before == Phase::Ready && now != Phase::Ready {
            self.outage_base = recoveries;
        }
        before == Phase::Reconnecting && now == Phase::Ready && recoveries == self.outage_base
    }
}

/// How a window process ended, as the reaper takes it.
#[derive(Debug, PartialEq, Eq)]
enum WindowEnd {
    /// It never reached its first frame (no display, no OpenGL): the user
    /// asked for a window and got none.
    CouldNotOpen,
    /// The user closed it.
    Closed,
    /// It ran and then died (its display went away, a crash, a signal).
    Died,
}

/// What a window's end does to the quit question it was to show.
#[derive(Debug, PartialEq, Eq)]
enum QuitQuestion {
    /// No question, or nothing to change.
    Keep,
    /// The user closed the window, or it died, without answering: the
    /// question goes, so the next tray Quit asks again (a second Quit
    /// quits only while the question shows).
    Withdraw,
    /// There was no window to ask in: Quit.
    Quit,
}

fn quit_question_after(end: &WindowEnd, asking: bool) -> QuitQuestion {
    match (end, asking) {
        (_, false) => QuitQuestion::Keep,
        (WindowEnd::CouldNotOpen, true) => QuitQuestion::Quit,
        (WindowEnd::Closed | WindowEnd::Died, true) => QuitQuestion::Withdraw,
    }
}

fn window_end(status: std::process::ExitStatus) -> WindowEnd {
    match status.code() {
        Some(0) => WindowEnd::Closed,
        Some(hushmic::window::EXIT_COULD_NOT_OPEN) => WindowEnd::CouldNotOpen,
        _ => WindowEnd::Died,
    }
}

/// What was held back while the window showed it goes out once the window
/// no longer does (focus went, another page, closed, a new window that
/// never took focus) if still so.
fn release_held(state: &hushmic::appstate::AppState) {
    notify::release_held(|p| still_so(p, state));
}

/// The user's first close of the window: watch the session it was in for a
/// logout (a close that belongs to one must not spend the one-time
/// notice), then wake the loop with the verdict.
fn arm_first_close(
    first_close: &mut notify::FirstClose,
    cfg: &Config,
    tx: &mpsc::Sender<Event>,
    session: Option<String>,
) {
    if first_close.closed(cfg.close_notice_sent, notify::enabled()) {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let session = session.or_else(notify::display_session);
            let _ = tx.send(Event::CloseNoticeDue(notify::session_stays(session)));
        });
    }
}

/// Whether a problem held back while the window showed it is still so,
/// judged on the loop's newest snapshot.
fn still_so(p: notify::Problem, s: &hushmic::appstate::AppState) -> bool {
    use hushmic::appstate::Phase;
    use notify::Problem as P;
    match p {
        P::Failure => {
            matches!(s.engine.phase, Phase::Down | Phase::Reconnecting) || !s.pipewire.reachable
        }
        P::Flapping => s.engine.flapping,
        P::Rerouted => s.mic.rerouted,
        P::MicStuck => s.mic.stuck,
        P::MicSilent => s.mic.silent,
        P::MicFallback => s.mic.fallback,
        P::DefaultReleased => s.apps.default_released.is_some(),
        P::Recovered | P::MicBack => false,
    }
}

/// Open the companion window on `page`, replacing an open one: Wayland
/// cannot raise another process's window, so a fresh one comes up on top.
/// It paints its first frame in `look` (the snapshot's), not in a default
/// that the first snapshot then corrects. What the old one left in the
/// runtime dir goes. Every open is one the user asked for (a launch, a
/// relaunch, Open HushMic), so one that fails says so.
/// Open the window (replacing one that is open). False when it could not
/// be started.
fn open_companion(
    win: &mut Option<Companion>,
    page: hushmic::appstate::Page,
    size: Option<[u16; 2]>,
    look: hushmic::appstate::DesktopLook,
    env: &[(String, String)],
    tx: &mpsc::Sender<Event>,
) -> bool {
    if close_companion(win) {
        hushmic::miccheck::remove_files();
    }
    let mut args = vec!["--window".to_string(), "--page".into(), page.word().into()];
    if let Some([w, h]) = size {
        args.extend(["--size".into(), format!("{w}x{h}")]);
    }
    args.extend([
        "--theme".into(),
        match look.effective_theme {
            hushmic::appstate::Theme::Dark => "dark",
            hushmic::appstate::Theme::Light => "light",
        }
        .into(),
    ]);
    if look.reduced_motion {
        args.push("--reduced-motion".into());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match spawn_child_window(&args, env) {
        Ok(child) => {
            eprintln!(
                "[hushmic] window opened on {} (pid {})",
                page.word(),
                child.id()
            );
            unretire_window(child.id());
            let (pid, tx) = (child.id(), tx.clone());
            std::thread::spawn(move || {
                std::thread::sleep(SETTLE_GRACE);
                let _ = tx.send(Event::WindowSettled(pid));
            });
            let session = Arc::new(OnceLock::new());
            let slot = session.clone();
            std::thread::spawn(move || {
                let _ = slot.set(notify::display_session());
            });
            *win = Some(Companion {
                child,
                spawned: Instant::now(),
                focused: false,
                page,
                settling: true,
                session,
                live: notify::LiveShows::Untold,
            });
        }
        Err(e) => {
            eprintln!("hushmic: could not open the window: {e}");
            notify_window_failed();
            sync_window_shows(win);
            return false;
        }
    }
    // a new window starts without focus
    sync_window_shows(win);
    true
}

/// What the tray's Quit does with `busy` files cleaning or saving, while
/// the window asks already (`asking`) or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayQuit {
    /// The window asks first (the quit sheet: "2 files are still cleaning.").
    Ask,
    /// Quit (stopping the files): nothing to stop, or a second Quit while
    /// the question shows.
    Quit,
}

fn tray_quit(busy: usize, asking: bool) -> TrayQuit {
    if busy > 0 && !asking {
        TrayQuit::Ask
    } else {
        TrayQuit::Quit
    }
}

/// The microphone a silent one's notice offers (UX panel round 2, A5):
/// the one other real microphone there is, or none (several, or none).
fn silent_mic_way_out<'a>(
    mics: &'a [pipewire::Source],
    silent: Option<&str>,
) -> Option<&'a pipewire::Source> {
    let mut others = mics.iter().filter(|m| Some(m.name.as_str()) != silent);
    match (others.next(), others.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

/// A microphone silent to an app (UX panel A4): only while HushMic runs
/// and an app records it; a silent mic nobody listens to is no news.
/// `silent` reads the meter, only then.
fn mic_silent_now(running: bool, app_records: bool, silent: impl FnOnce() -> bool) -> bool {
    running && app_records && silent()
}

/// Whether the window shows a quit or turn-off question already, the
/// daemon's (files cleaning, `files_asking`) or the window's (`ask`): the
/// tray's Quit or Off then brings it to the front and does nothing more
/// (review N-M1: a second Quit asked the other question, a third quit).
fn tray_question_up(files_asking: bool, ask: Option<hushmic::appstate::Ask>) -> bool {
    files_asking || ask.is_some()
}

/// Whether the tray's Quit or Off (`cmd`) has the window ask first: an
/// app uses HushMic (Off: and it is on), and the window does not ask that
/// already (while a question shows, [`tray_question_up`] comes first).
fn tray_asks(
    cmd: &TrayCmd,
    in_use: bool,
    asking: Option<hushmic::appstate::Ask>,
    enabled: bool,
) -> bool {
    use hushmic::appstate::Ask;
    let asks = match cmd {
        TrayCmd::AskQuit => Ask::Quit,
        TrayCmd::AskOff if enabled => Ask::TurnOff,
        _ => return false,
    };
    in_use && asking != Some(asks)
}

/// After Off or Quit while an app used HushMic: one notification that it
/// now hears the microphone directly (UX panel A3). Quit waits a moment
/// for it to go out, as the process ends right after.
fn notify_direct_after(hub: &watch::Hub, quit: bool) -> Option<String> {
    let state = hub.last()?;
    let body = notify::direct_after(&state)?;
    let summary = if quit {
        tr!("notify-direct-quit-summary")
    } else {
        tr!("notify-direct-off-summary")
    };
    if quit {
        notify::send_and_wait(
            notify::Slot::Power,
            "audio-input-microphone",
            &summary,
            &body,
            Duration::from_secs(1),
        );
    } else {
        // one click turns it back on (UX panel round 2, A5)
        notify::send_with_actions(
            notify::Slot::Power,
            "audio-input-microphone",
            &summary,
            &body,
            &[("turn-on", &tr!("notify-turn-on"))],
        );
    }
    Some(body)
}

/// What a click on a "HushMic is off" or "No sound from …" notification
/// does (UX panel round 2, A5): its own button acts ("Turn on", "Use
/// <mic>"); a click on the bubble itself (`default`) only opens the
/// window (A107 SF-N1). None for the other topics.
fn notice_command(slot: notify::Slot, key: &str) -> Option<TrayCmd> {
    match (slot, key) {
        (notify::Slot::Power, "turn-on") => Some(TrayCmd::SetMode(Some(RunMode::Suppress))),
        (notify::Slot::Power, _) => Some(TrayCmd::Open),
        (notify::Slot::Silence, k) => Some(match k.strip_prefix("use-mic:") {
            Some(mic) => TrayCmd::SelectMic(Some(mic.to_string())),
            None => TrayCmd::Open,
        }),
        _ => None,
    }
}

/// The page to open on: a window that is open is re-created where it was;
/// otherwise a fresh open (Live, Files while `files` clean or wait to be
/// saved, or first run while under way).
fn companion_page(
    win: &mut Option<Companion>,
    ui: &hushmic::appstate::UiState,
    files: bool,
) -> hushmic::appstate::Page {
    let open = win
        .as_mut()
        .is_some_and(|c| matches!(c.child.try_wait(), Ok(None)));
    if open {
        windowstate::reopen_page(ui)
    } else {
        windowstate::fresh_page(ui, files)
    }
}

/// A window the user asked for could not open: say so, and how to test
/// the microphone without one.
fn notify_window_failed() {
    notify::send(
        Slot::Status,
        "dialog-error",
        "HushMic",
        &format!(
            "{}\n{}",
            tr!("notify-window-failed-body"),
            tr!("notify-window-failed-miccheck")
        ),
    );
}

/// The look a window opens with. The desktop reader starts with the first
/// window, a headless daemon's too (by then the session and its portal are
/// up; the unit never wakes the portal early), and that first open waits
/// briefly for its first answer: a running portal answers in milliseconds,
/// one being started gets 250 ms, then the snapshots catch up. Later opens
/// have it read again (the kdeglobals file is not followed otherwise).
/// `env` is the launch's (a forwarded one names the desktop, which a
/// daemon started before the session does not know itself).
fn window_look(
    reader: &mut Option<hushmic::desktop::Reader>,
    tx: &mpsc::Sender<Event>,
    watched: &mut Watched,
    cfg: &Config,
    env: &[(String, String)],
) -> hushmic::appstate::DesktopLook {
    let desktop = env
        .iter()
        .find(|(k, _)| k == "XDG_CURRENT_DESKTOP")
        .map(|(_, v)| v.as_str());
    match reader {
        Some(r) => {
            if let Some(d) = desktop {
                r.desktop(d);
            }
            r.refresh();
        }
        None => {
            let tx = tx.clone();
            let r = hushmic::desktop::spawn(
                move |look| {
                    let _ = tx.send(Event::Desktop(look));
                },
                desktop,
            );
            if let Some(look) = r.first(Duration::from_millis(250)) {
                watched.desktop = look;
            }
            *reader = Some(r);
        }
    }
    hushmic::snapshot::desktop_look(cfg, watched.desktop)
}

/// SIGTERM a live companion window and reap it. True when there was a
/// live one to close.
fn close_companion(win: &mut Option<Companion>) -> bool {
    let mut closed = false;
    if let Some(Companion { child, .. }) = win.as_mut() {
        retire_window(child.id());
        if matches!(child.try_wait(), Ok(None)) {
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
            let _ = child.wait();
            closed = true;
        }
    }
    *win = None;
    closed
}

/// The notification-driven mic test (record a few seconds, play both
/// takes): `hushmic miccheck`, the way to test where no window can open,
/// self-announcing via notifications. Err carries why it could not start
/// (also notified).
fn start_fallback_mictest(
    cfg: &Config,
    testing: &mut bool,
    mictest_cancel: &mut Option<Arc<AtomicBool>>,
    tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
    let dump = pipewire::pw_dump_within(pipewire::CALL_BUDGET);
    let node_present = dump.as_deref().map(|d| pipewire::chain_graph(d).up());
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
        Err(msg) => {
            notify::send(
                Slot::MicTest,
                "audio-input-microphone",
                &tr!("notify-mictest-title"),
                &msg,
            );
            Err(msg)
        }
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
            Ok(())
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
        // The daemon runs elsewhere: an absolute path, as a JSON string (it
        // may hold spaces).
        ["files", "add", path] => {
            let p = std::path::absolute(path).unwrap_or_else(|_| path.into());
            let json = serde_json::to_string(&p.to_string_lossy()).unwrap_or_default();
            let words = ["files".to_string(), "add".into(), json];
            return control::client_run(&words);
        }
        ["files", "replace", id, path] => {
            let p = std::path::absolute(path).unwrap_or_else(|_| path.into());
            let json = serde_json::to_string(&p.to_string_lossy()).unwrap_or_default();
            let words = ["files".to_string(), "replace".into(), id.to_string(), json];
            return control::client_run(&words);
        }
        ["files", "save", id, dir] => {
            let p = std::path::absolute(dir).unwrap_or_else(|_| dir.into());
            let json = serde_json::to_string(&p.to_string_lossy()).unwrap_or_default();
            let words = ["files".to_string(), "save".into(), id.to_string(), json];
            return control::client_run(&words);
        }
        // `files folder none` goes as it is: next to the original again
        ["files", "folder", dir] if *dir != "none" => {
            let p = std::path::absolute(dir).unwrap_or_else(|_| dir.into());
            let json = serde_json::to_string(&p.to_string_lossy()).unwrap_or_default();
            let words = ["files".to_string(), "folder".into(), json];
            return control::client_run(&words);
        }
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
            let Some(all) = pipewire::sources_snapshot() else {
                return (
                    1,
                    "hushmic: PipeWire did not answer (is it running?)".into(),
                );
            };
            let sources = pipewire::filter_real(&all);
            return (
                0,
                config_cli::render_devices(&sources, configured.as_deref(), json),
            );
        }
        #[cfg(feature = "speakers")]
        ["devices", "outputs"] | ["devices", "outputs", "--json"] => {
            let json = words.len() == 3;
            let status: Vec<String> = ["status", "--json"].iter().map(|s| s.to_string()).collect();
            let configured = match control::client_run(&status) {
                (0, out) => serde_json::from_str::<serde_json::Value>(&out)
                    .ok()
                    .and_then(|v| {
                        v["speakers"]["output"]["configured"]
                            .as_str()
                            .map(str::to_string)
                    }),
                _ => Config::load().speakers().output,
            };
            let Some(dump) = pipewire::pw_dump() else {
                return (
                    1,
                    "hushmic: PipeWire did not answer (is it running?)".into(),
                );
            };
            let g = hushmic::speakers::graph::Graph::read_for(&dump, Config::load().mic.as_deref());
            let outputs = g.offered(configured.as_deref(), None);
            return (
                0,
                hushmic::speakers::render_outputs(&outputs, configured.as_deref(), json),
            );
        }
        ["devices", ..] => return (1, hushmic::cli_help::usage("devices").unwrap_or_default()),
        #[cfg(feature = "speakers")]
        ["speakers"] => {
            let show: Vec<String> = vec!["speakers".into()];
            return match control::client_run(&show) {
                (2, _) => (0, hushmic::speakers::render_show(&Config::load(), None)),
                r => r,
            };
        }
        #[cfg(feature = "speakers")]
        ["speakers", rest @ ..] => {
            let set = |key: &str, value: &str| -> Vec<String> {
                ["config", "set", key, value]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            };
            let args = match rest {
                ["on"] => set("speakers.enabled", "true"),
                ["off"] => set("speakers.enabled", "false"),
                ["strength", v] => set("speakers.attn_limit", v),
                ["output", v] => set("speakers.output", v),
                _ => return (1, hushmic::cli_help::usage("speakers").unwrap_or_default()),
            };
            let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let (code, out) = cli_config_fallback(&args, &refs);
            if let (0, ["output", name]) = (code, rest) {
                let graph = pipewire::pw_dump().map(|d| {
                    hushmic::speakers::graph::Graph::read_for(&d, Config::load().mic.as_deref())
                });
                if let Some(w) = hushmic::speakers::output_warning(name, graph.as_ref()) {
                    return (0, format!("{}\n{w}", out.trim_end()));
                }
            }
            return (code, out);
        }
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
                    2 => config_cli::offline_set(
                        "autostart",
                        "false",
                        &Paths::resolve().model_dir,
                        false,
                    )
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
        ["service", ..] => return (1, hushmic::cli_help::usage("service").unwrap_or_default()),
        // the apps that now hear the raw microphone go to stderr, the mode
        // word to stdout (UX panel A1)
        ["mode", "off"] => {
            let (code, out) = control::client_run(args);
            let (word, notes) = control::split_notes(&out);
            for n in notes {
                eprintln!("{n}");
            }
            return (code, word);
        }
        // a script reads one answer either way (UX panel round 2, A11)
        ["status", "--json"] => {
            let (code, out) = control::client_run(args);
            if code != 2 {
                return (code, out);
            }
            eprintln!("{}", out.trim_end());
            println!(r#"{{"running":false}}"#);
            return (2, String::new());
        }
        ["quit"] | ["quit", "--force"] => return quit_and_wait(args),
        ["miccheck"] => return miccheck_and_wait(args),
        _ => {}
    }
    let (code, out) = cli_config_fallback(args, &words);
    if let (0, ["config", "set", "mic", name]) = (code, words.as_slice()) {
        let usable = pipewire::sources_snapshot().map(|v| pipewire::filter_real(&v));
        if let Some(w) = config_cli::mic_warning(name, usable.as_deref()) {
            return (0, format!("{}\n{w}", out.trim_end()));
        }
    }
    (code, out)
}

/// `cli_dispatch`'s control commands: through the running instance, and
/// `config` on the file when none runs.
fn cli_config_fallback(args: &[String], words: &[&str]) -> (i32, String) {
    use hushmic::config_cli;
    let (code, out) = control::client_run(args);
    if code == 0 && matches!(words, ["config", "set", "autostart", ..]) {
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
    // model/attn_limit are the ones the device a start would use runs with.
    let device = || config_cli::offline_device(&cfg);
    let res = match words {
        ["config"] => config_cli::offline_get(&cfg, None, false, device().as_deref()),
        ["config", "--json"] => config_cli::offline_get(&cfg, None, true, device().as_deref()),
        ["config", "get", k] => config_cli::offline_get(&cfg, Some(k), false, device().as_deref()),
        ["config", "get", k, "--json"] => {
            config_cli::offline_get(&cfg, Some(k), true, device().as_deref())
        }
        ["config", "set", "--defaults", k, rest @ ..] if !rest.is_empty() => {
            config_cli::offline_set(k, &rest.join(" "), &model_dir, true)
        }
        ["config", "set", k, rest @ ..] if !rest.is_empty() && !k.starts_with("--") => {
            config_cli::offline_set(k, &rest.join(" "), &model_dir, false)
        }
        _ => Err(out),
    };
    match res {
        Ok(s) => (0, s),
        Err(e) => (1, e),
    }
}

/// Writes `cfg` off the loop; a write that fails comes back as
/// [`Event::Unwritten`] (a notice), for changes whose answer went out
/// once they were applied.
fn save_noting(saver: &hushmic::config::Saver, cfg: &Config, tx: &mpsc::Sender<Event>) {
    let tx = tx.clone();
    saver.save_then(cfg, move |w| {
        if let Err(e) = w {
            let _ = tx.send(Event::Unwritten(e));
        }
    });
}

/// A change's outcome joined with its write's: every error, "; ".
fn with_write(
    applied: Result<(), String>,
    written: hushmic::config::Written,
) -> Result<(), String> {
    match (applied, written) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
        (Err(a), Err(w)) => Err(format!("{a}; {w}")),
    }
}

/// The one path that changes settings from outside the tray menu: diff
/// and apply only what changed. `cfg` is replaced by `new` even when the
/// chain restart fails (the tray does the same: a bad setting is visible,
/// not silently reverted). Every error (autostart entry, chain restart)
/// is reported, joined with "; ". The caller persists it through the
/// [`Saver`](hushmic::config::Saver) and reports after that write
/// ([`with_write`]).
#[allow(clippy::too_many_arguments)]
fn apply_config(
    cfg: &mut Config,
    new: Config,
    controller: &mut Controller,
    apply: &dyn Fn(&mut Controller, &Config) -> Result<(), String>,
    testing: bool,
    mictest_cancel: &Option<Arc<AtomicBool>>,
    gate: &mut FailureGate,
    fallback_ended: bool,
) -> Result<(), String> {
    let d = hushmic::config_cli::diff(cfg, &new);
    let restart = hushmic::controller::settings_restart(d.chain, fallback_ended);
    let mut errors: Vec<String> = Vec::new();
    if d.notifications {
        notify::set_enabled(new.notifications);
    }
    if d.autostart {
        if let Err(e) = autostart::set_autostart(new.autostart) {
            errors.push(format!("the autostart entry could not be written: {e}"));
        }
    }
    // The smallest change (a strength edit stays live), except after a
    // recovery fallback: the pick must restart even with nothing changed.
    let chain_result = if restart && new.enabled {
        if fallback_ended {
            invalidate_mic_consumers(testing, mictest_cancel);
            apply(controller, &new)
        } else {
            apply_mic(controller, &new, testing, mictest_cancel)
        }
    } else {
        Ok(())
    };
    *cfg = new;
    if let Err(e) = &chain_result {
        eprintln!("hushmic: enable failed: {e}");
        if gate.on_enable_error(e, true) {
            notify::send_problem(notify::Problem::Failure, "dialog-error", &fail_summary(), e);
        }
        errors.push(format!("the microphone could not restart: {e}"));
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

/// `(mic, device)`: the device a start would take its settings from, and
/// the configured `mic` it was resolved for.
type TargetCache = Option<(Option<String>, Option<String>)>;

/// The device whose model and strength the tray, `status` and edits refer
/// to: the running chain's (what its conf was rendered with), else the one
/// a start would pick — from the cache the Tick keeps while no chain runs,
/// probed only when that is missing or was resolved for another `mic`.
/// None = no device (the defaults).
fn settings_device(
    cfg: &Config,
    controller: &Controller,
    cache: &mut TargetCache,
) -> Option<String> {
    if let Some(p) = controller.active_profile() {
        // following the default: the device it captures from now (UX
        // panel A2)
        return controller
            .in_use()
            .map(str::to_string)
            .or_else(|| p.device.clone());
    }
    if let Some((mic, device)) = cache.as_ref() {
        if *mic == cfg.mic {
            return device.clone();
        }
    }
    let device = pipewire::resolve_chain_target(cfg.mic.as_deref(), None).profile_device;
    *cache = Some((cfg.mic.clone(), device.clone()));
    device
}

/// After any settings change (tray menu or `config set`): the tray gets the
/// full state as the loop knows it now. The graph (mic list, node) is not
/// read here: that waits for the session manager, and a mute must never
/// wait behind it. Callers start a graph probe after this. The one place
/// the post-change tray closure lives.
fn refresh_tray(
    handle: &TrayLink,
    cfg: &Config,
    controller: &mut Controller,
    known_mics: &[pipewire::Source],
    last_node_present: Option<bool>,
    engine: &mut EngineCache,
    target_cache: &mut TargetCache,
) {
    let status = compute_status(cfg, controller, last_node_present);
    let new_mics = known_mics.to_vec();
    let snapshot = cfg.clone();
    let profile = cfg.profile_for(settings_device(cfg, controller, target_cache).as_deref());
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
            t.profile = profile;
            t.icon_style = icon_style;
            t.mics = new_mics;
            t.status = status;
            t.mode = mode_now;
            t.engine = engine_now.0;
            t.engine_light_configured = engine_now.1;
        })
        .is_some();
    engine.record(engine_now, landed);
}

/// The loop's state that exists for the window (the `watch` snapshot), on
/// top of what the tray already keeps.
struct Watched {
    lifecycle: Lifecycle,
    /// Why the next spawn happens (set by the handler that is about to
    /// start one; the loop head spends it).
    start_why: Start,
    /// Where each device stood in the last list it was in: an unplugged
    /// chosen mic keeps its place in the list.
    slots: HashMap<String, usize>,
    system_default: Option<String>,
    /// The device feeding the chain right now (its capture link), for
    /// when neither the config nor the default says which one it is.
    feeder: Option<String>,
    /// Since when the graph reads have seen no device on the capture link.
    unlinked_since: Option<Instant>,
    using: Vec<AppRef>,
    /// The apps' streams recording HushMic at the last graph read.
    users: Vec<pipewire::StreamNode>,
    /// The streams Off moved off HushMic, moved back when it is on again
    /// (UX panel A1), and since when it is on with them to move.
    off_moved: Vec<pipewire::StreamNode>,
    return_from: Option<Instant>,
    /// A graph read is asked for when the return is due.
    return_asked: bool,
    direct: Vec<DirectUse>,
    direct_watch: apps::DirectWatch,
    /// Display names from the desktop entries, for apps that report none.
    names: apps::Names,
    /// Where the user moved the default away from HushMic to.
    released_to: Option<String>,
    shortcuts: ShortcutsState,
    /// Consecutive failed pw-dump probes (one is churn, two is an outage).
    pw_failures: u32,
    /// When the last snapshot of the graph was read.
    graph_at: Option<Instant>,
    ui: hushmic::appstate::UiState,
    /// What the state file holds (as last written by this run).
    ui_saved: Option<hushmic::appstate::UiState>,
    /// What the desktop says about its look (light/dark, less motion).
    desktop: hushmic::desktop::Look,
    /// When the engine's current run began, from its meter.
    runs: hushmic::usemark::Watch,
    /// When the current use began (for the window's history).
    use_mark: hushmic::usemark::UseMark,
    /// The tray's Quit or Off waits for the window's answer (an app uses
    /// HushMic): `AppState::ask`.
    ask: Option<hushmic::appstate::Ask>,
    /// Streams linked into HushMic's capture, or taking sound from its
    /// streams, by a stale node id (see `pipewire::ChainGraph::intruders`).
    guard: pipewire::IntruderGuard,
}

impl Watched {
    /// What a pw-dump snapshot says that the window shows: PipeWire
    /// answers, the device "System default" means, who records HushMic.
    fn observe_graph(&mut self, dump: &str, controller: &mut Controller) {
        self.graph_at = Some(Instant::now());
        self.pw_failures = 0;
        self.feeder = pipewire::parse_feeders(dump, "hushmic_input")
            .into_iter()
            .find(|n| hushmic::config::can_own_profile(n));
        self.unlinked_since = match self.feeder {
            Some(_) => None,
            None => self.unlinked_since.or(Some(Instant::now())),
        };
        // What "System default" means now: the default the chain follows,
        // or with that one gone, the device the chain fell back to (UX
        // panel A2, A8: the window, the tray and the settings name it).
        let followed = pipewire::followed_default(dump, controller.prior_default());
        controller.observe_in_use(followed.clone().or_else(|| self.feeder.clone()));
        self.system_default = followed.or_else(|| controller.in_use().map(str::to_string));
        // Who records HushMic: the apps, and whether anything else runs it.
        let readers = pipewire::parse_readers(dump, "hushmic_source");
        self.using = apps::using(&readers, &apps::is_ours, &self.names);
        self.users = apps::app_streams(&readers, &apps::is_ours);
        self.use_mark.observe(
            hushmic_shm::monotonic_ms(),
            !self.using.is_empty(),
            apps::others_running(&readers, &apps::is_ours),
            controller.host_generation(),
            self.runs.began_after(),
        );
    }

    /// Off: the streams recording HushMic now (and any a recent Off moved
    /// that are not back yet) go back once it is on again (UX panel A1).
    fn turned_off(&mut self) {
        for s in std::mem::take(&mut self.users) {
            if !self.off_moved.contains(&s) {
                self.off_moved.push(s);
            }
        }
        self.return_from = None;
    }

    /// On again: the streams Off moved are due back.
    fn turned_on(&mut self) {
        if !self.off_moved.is_empty() {
            self.return_from = Some(Instant::now());
            self.return_asked = false;
        }
    }

    /// The streams Off moved off HushMic go back onto it, once its node is
    /// there and [`RETURN_AFTER`] has passed (the session manager moves the
    /// streams that follow the default by itself; those are left to it).
    /// Off the loop: each move waits for the session manager.
    fn return_streams(&mut self, dump: &str, tx: &mpsc::Sender<Event>) {
        let Some(from) = self.return_from else {
            return;
        };
        if from.elapsed() > RETURN_GIVE_UP {
            eprintln!("[hushmic] HushMic's microphone did not come back; apps stay where they are");
            self.off_moved.clear();
            self.return_from = None;
            return;
        }
        // its node must be there (the move names it)
        if pipewire::parse_node_serial(dump, "hushmic_source").is_none() {
            return;
        }
        let wait = RETURN_AFTER.saturating_sub(from.elapsed());
        if !wait.is_zero() {
            if !std::mem::replace(&mut self.return_asked, true) {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(wait);
                    let _ = tx.send(Event::Tick);
                });
            }
            return;
        }
        let ids = apps::to_return(
            &self.off_moved,
            &pipewire::parse_capture_streams(dump),
            &self.users,
        );
        self.off_moved.clear();
        self.return_from = None;
        if ids.is_empty() {
            return;
        }
        eprintln!(
            "[hushmic] moving {} app stream(s) back onto HushMic after Off",
            ids.len()
        );
        let tx = tx.clone();
        std::thread::spawn(move || {
            for id in ids {
                if !pipewire::move_stream(id, "hushmic_source") {
                    eprintln!("[hushmic] could not move stream {id} back (pw-metadata error)");
                }
            }
            let _ = tx.send(Event::Tick);
        });
    }

    /// A pw-dump probe failed. One failure is graph churn, two mean
    /// PipeWire is unreachable: the second probe comes early instead of a
    /// tick later, so the window says so within seconds.
    fn pw_failed(&mut self, tx: &mpsc::Sender<Event>) {
        self.pw_failures = self.pw_failures.saturating_add(1);
        if self.pw_failures == 1 {
            schedule_early_tick(tx);
        }
    }

    /// Persist the window's state when it changed; a failure costs only
    /// the restore.
    fn save_ui(&mut self) {
        if self.ui_saved.as_ref() == Some(&self.ui) {
            return;
        }
        match windowstate::save(&self.ui) {
            Ok(()) => self.ui_saved = Some(self.ui.clone()),
            Err(e) => eprintln!("[hushmic] could not save the window state: {e}"),
        }
    }
}

/// The device the running chain captures from, as the window's `watch`
/// and `status` both name it: its capture link in the last graph read
/// (the truth, also right after a hotplug the controller has not acted
/// on yet), else the pin it was started with, else the default it
/// follows, else the device it was started on. None without a chain.
fn linked_mic(w: &Watched, controller: &mut Controller) -> Option<String> {
    if !controller.is_running() {
        return None;
    }
    w.feeder
        .clone()
        .or_else(|| controller.active_mic().map(str::to_string))
        .or_else(|| w.system_default.clone())
        .or_else(|| controller.active_profile().and_then(|p| p.device.clone()))
}

/// The chosen microphone is not what the chain captures from because it
/// is gone: the chain was moved to a stand-in, or the chosen one has
/// left the graph. Not while the capture is already back on it (a replug
/// WirePlumber relinked before the recovery's restart), and not a
/// capture another tool moved away from a present mic (that is
/// `rerouted`).
fn on_fallback(
    cfg: &Config,
    controller: &mut Controller,
    known_mics: &[pipewire::Source],
    linked: Option<&str>,
) -> bool {
    cfg.enabled
        && controller.is_running()
        && cfg.mic.as_deref().is_some_and(|chosen| {
            fallen_back(
                chosen,
                controller.active_mic(),
                linked,
                known_mics.iter().any(|m| m.name == chosen),
            )
        })
}

/// [`on_fallback`] for a running chain: `started_on` is the pin it was
/// started with (None: the stand-in, the default), `linked` what feeds it.
fn fallen_back(
    chosen: &str,
    started_on: Option<&str>,
    linked: Option<&str>,
    present: bool,
) -> bool {
    linked != Some(chosen) && (started_on != Some(chosen) || !present)
}

/// The microphone that stands in when the chosen one went: the one the
/// session manager already moved the capture to, never the chosen one.
/// The fallback's start takes its settings (UX panel round 2, A9).
fn stand_in(chosen: Option<&str>, feeder: Option<&str>) -> Option<String> {
    feeder
        .filter(|f| Some(*f) != chosen && hushmic::config::can_own_profile(f))
        .map(str::to_string)
}

/// A microphone as the window names it (its display name, from the
/// devices seen; else PipeWire's description; else its node name), or
/// "the system default" for None.
fn mic_label(
    cfg: &Config,
    controller: &Controller,
    known_mics: &[pipewire::Source],
    nodes: Option<&[pipewire::Source]>,
    dev: Option<&str>,
) -> String {
    let Some(dev) = dev else {
        return tr!("notify-system-default");
    };
    controller
        .mic_name(cfg, known_mics, dev)
        .or_else(|| {
            nodes
                .and_then(|v| pipewire::description_of(v, dev))
                .map(str::to_string)
        })
        .unwrap_or_else(|| dev.to_string())
}

/// The snapshot the window sees, from the loop's state.
#[allow(clippy::too_many_arguments)]
fn app_state(
    w: &Watched,
    cfg: &Config,
    controller: &mut Controller,
    known_mics: &[pipewire::Source],
    node_present: Option<bool>,
    handle: &TrayLink,
    headless: bool,
    unmute_to: RunMode,
    target_cache: &mut TargetCache,
    rerouted: bool,
    mic_stuck: bool,
    mic_silent: bool,
    gate: &FailureGate,
) -> hushmic::appstate::AppState {
    let running = controller.is_running();
    let generation = hushmic::diagnostics::engine_generation();
    // The running chain's engine meter: the window's live levels, and a
    // new path means a new chain.
    let meter = controller.meter_path().map(|p| p.display().to_string());
    let profile = cfg.profile_for(settings_device(cfg, controller, target_cache).as_deref());
    let active_mic = linked_mic(w, controller);
    let fallback = on_fallback(cfg, controller, known_mics, active_mic.as_deref());
    snapshot::build(&snapshot::Facts {
        cfg,
        headless,
        flatpak: hushmic::sandbox::is_flatpak(),
        mode: controller.mode(),
        unmute_to,
        phase: w.lifecycle.phase(
            cfg.enabled,
            running,
            generation,
            node_present,
            Instant::now(),
        ),
        generation,
        tier: running.then(hushmic::diagnostics::engine_tier).flatten(),
        configured_light: controller.active_model_is_light(),
        error: w.lifecycle.error(),
        flapping: gate.flapping(),
        recovered: gate.recoveries(),
        meter: meter.as_deref(),
        use_began_ms: w.use_mark.mark(),
        profile: &profile,
        active_mic: active_mic.as_deref(),
        fallback,
        mics: known_mics,
        seen: controller.seen_mics(),
        slots: &w.slots,
        system_default: w.system_default.as_deref(),
        rerouted,
        mic_stuck,
        mic_silent,
        using: &w.using,
        direct: &w.direct,
        can_set_default: pipewire::can_set_default(),
        default_released: w.released_to.as_deref(),
        shortcuts: &w.shortcuts,
        tray: tray_host(handle),
        pipewire_reachable: w.pw_failures < 2,
        live_capture: None,
        ui: &w.ui,
        desktop: w.desktop,
    })
}

fn usage_text() -> String {
    let text = "usage: hushmic                 start the tray and open the window
                               (an already-running instance opens it instead)
       hushmic --tray          run the system-tray app (no window; autostart)
       hushmic --headless      run without a tray icon or window (systemd unit)
       hushmic --enable-once   enable the mic until terminated (no watchdog, no CLI socket)
       hushmic --window [--page PAGE] [--mock SCENARIO] [--size WxH]
                    [--theme dark|light] [--reduced-motion]
                               the window alone (the daemon opens it; --mock shows a sample state)
       hushmic --version, -V   print the version and install paths
       hushmic --doctor        print a diagnostics report (exits 1 on problems)
       hushmic --help, -h      this text (COMMAND --help or help COMMAND: that
                               command's own help)

control (talks to the running instance):
       hushmic status [--json]      what it is doing
       hushmic mode [STATE]         print or set: suppress|bypass|mute|off
       hushmic toggle mute|bypass   hotkey-friendly overlay toggle
       hushmic unmute-to suppress|bypass
                                    what unmuting returns to (not muted: the
                                    next mute's)
       hushmic quit [--force]       stop it; returns once it is gone (the previous
                                    default mic is back). Refuses while files are
                                    cleaning or cleaned and not saved (--force: quit)
       hushmic watch                its state as JSON lines, one per change
       hushmic refresh              re-read PipeWire now
       hushmic retry                retry a microphone that is down, now
       hushmic shortcuts setup      open the desktop's shortcut dialog
       hushmic miccheck             record a few seconds, play them back raw, then
                                    cleaned (no window needed; notifications guide);
                                    returns when both have played
       hushmic notice hide APP [NAME] | show APP
                                    leave APP out of the apps listed as recording
                                    the microphone directly, or list it again
       hushmic profile reset DEVICE forget a microphone's own settings

recordings:
       hushmic open-files FILE|FOLDER… [-r]
                                    put them on the Files page (starts HushMic
                                    if it is not running); -r: subfolders too
       hushmic clean FILE|FOLDER… [-r] [--strength gentle|medium|strong|maximum]
                    [--model quality|light] [--track N|all] [--out DIR] [--json]
                    [--quiet]
                                    clean them; each saves as \"NAME (cleaned).EXT\"
                                    next to it (exit 1 if any could not be)

settings (applied live while running, saved to the file otherwise):
       hushmic config [--json]           every key = value
       hushmic config get KEY [--json]
       hushmic config set [--defaults] KEY VALUE
       hushmic config path
       hushmic devices [--json]          microphones usable as `mic`
       hushmic service install|uninstall systemd user unit for this install
  keys: mic model attn_limit set_default autostart tray tray_icon notifications inference
        voice_hold appearance

exit codes: 0 ok, 1 usage error or failed, 2 not running (control commands),
            3 already running (--tray, --headless, --enable-once; 0 under systemd)
"
    .to_string();
    #[cfg(feature = "speakers")]
    let text = text
        .replace(
            "       hushmic service install",
            "       hushmic devices outputs [--json]  outputs usable as `speakers output`
       hushmic speakers [on|off]         the HushMic Speakers output: what it does,
                                         or switch it
       hushmic speakers strength maximum|strong|medium|gentle|DB
       hushmic speakers output default|NAME
       hushmic service install",
        )
        .replace(
            "        appearance\n",
            "        appearance speakers.enabled speakers.output speakers.attn_limit\n",
        );
    text
}

/// `hushmic COMMAND --help` (and `hushmic help COMMAND`): that command's
/// lines of the help, its keys for `config`, and the exit codes. None for
/// a word that is not a command.
fn command_help(cmd: &str) -> Option<String> {
    match cmd {
        "clean" => return Some(hushmic::files::cli::HELP.to_string()),
        "open-files" => return Some(hushmic::files::cli::OPEN_FILES_HELP.to_string()),
        "--window" => return Some(window_help()),
        _ => {}
    }
    // each command's own (F12); the extract below is the flags'
    if let Some(h) = hushmic::cli_help::help(cmd) {
        return Some(h);
    }
    let text = usage_text();
    let mut lines: Vec<&str> = Vec::new();
    let mut taking = false;
    let mut keys = false;
    for line in text.lines() {
        let body = line.strip_prefix("usage: ").unwrap_or(line);
        let t = body.trim_start();
        if let Some(rest) = t.strip_prefix("hushmic ") {
            taking = rest.split([' ', ',']).next() == Some(cmd);
        } else if t.starts_with("keys:") {
            keys = cmd == "config";
            taking = false;
        } else if !body.starts_with("        ") {
            taking = false;
            keys = false;
        }
        if taking {
            // the command lines are indented 7, their continuations deeper
            lines.push(body.get(7..).unwrap_or(t));
        } else if keys {
            lines.push(body.get(2..).unwrap_or(t));
        }
    }
    if lines.is_empty() {
        return None;
    }
    let codes: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("exit codes:"))
        .collect();
    let mut out = String::from("usage:\n");
    for l in lines {
        out.push_str(&format!("  {l}\n"));
    }
    out.push('\n');
    for l in codes {
        out.push_str(l);
        out.push('\n');
    }
    Some(out)
}

/// How long `hushmic quit` waits for the instance to be gone: the unit's
/// own stop limit (TimeoutStopSec) and a little.
const QUIT_WAIT: Duration = Duration::from_secs(27);

/// `hushmic quit`: returns once the instance is gone (its lock is free),
/// so the default microphone is back and its children are reaped; `quit
/// && hushmic --headless` does not race it.
fn quit_and_wait(args: &[String]) -> (i32, String) {
    let (code, out) = control::client_run(args);
    if code != 0 {
        return (code, out);
    }
    let path = lock::default_lock_path();
    let since = std::time::Instant::now();
    while since.elapsed() < QUIT_WAIT {
        // taken for a moment, and let go at once
        if let Ok(Some(held)) = lock::try_lock(&path) {
            drop(held);
            return (0, "stopped".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (
        1,
        format!(
            "hushmic was asked to stop but still runs after {} s",
            QUIT_WAIT.as_secs()
        ),
    )
}

/// `hushmic miccheck`: says it records, and returns when the check is
/// over (both takes played), saying how it ended.
fn miccheck_and_wait(args: &[String]) -> (i32, String) {
    use std::io::Write;
    let (code, out) = control::client_run(args);
    if code != 0 {
        return (code, out);
    }
    let _ = writeln!(std::io::stdout(), "{}", out.trim_end());
    let state = ["miccheck".to_string(), "state".into()];
    // the record, two plays and their margins (mictest's own bounds)
    let limit = Duration::from_secs(hushmic::mictest::RECORD_SECS * 3 + 20);
    let since = std::time::Instant::now();
    while since.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(250));
        match control::client_run(&state) {
            (0, s) if s.trim() == "running" => continue,
            (0, s) => return miccheck_end(s.trim()),
            (2, _) => return (1, "hushmic stopped during the mic check".into()),
            // an instance from before `miccheck state`: it cannot say
            (_, _) => return (0, String::new()),
        }
    }
    (1, "the mic check did not finish".into())
}

/// The last line of `hushmic miccheck` for how the check ended.
fn miccheck_end(state: &str) -> (i32, String) {
    match state {
        "done" => (
            0,
            "done: you heard the raw take, then the cleaned one".into(),
        ),
        "cancelled" => (1, "the mic check was cancelled (a setting changed)".into()),
        s => (
            1,
            format!(
                "the mic check failed: {}",
                s.strip_prefix("failed ").unwrap_or(s)
            ),
        ),
    }
}

/// `hushmic retry`'s answer: retrying only when something is down.
fn retry_reply(enabled: bool, down: bool) -> &'static str {
    match (enabled, down) {
        (false, _) => "HushMic is off: nothing to retry (hushmic mode suppress turns it on)",
        (true, true) => "retrying",
        (true, false) => "nothing is down: the HushMic microphone is working",
    }
}

/// What a help request asks for.
#[derive(Debug, PartialEq)]
enum Help {
    /// Not one: run the command.
    None,
    Text(String),
    /// `help` for a word that is not a command (exit 1).
    Unknown(String),
}

/// `--help`/`-h` (alone, or with options: the whole help), `help`,
/// `help COMMAND` and `COMMAND … --help` (that command's lines).
/// `clean` reads its own options (`clean FILE --help` is its help too).
fn help_request(args: &[String]) -> Help {
    let is_help = |a: &String| a == "--help" || a == "-h";
    let unknown =
        |w: &str| Help::Unknown(format!("hushmic: no command '{w}' (see hushmic --help)"));
    match args {
        [] => Help::None,
        [h] if is_help(h) || h == "help" => Help::Text(usage_text()),
        [h, cmd] if h == "help" => command_help(cmd).map_or_else(|| unknown(cmd), Help::Text),
        [cmd, rest @ ..] if cmd == "clean" && rest.iter().any(is_help) => {
            Help::Text(hushmic::files::cli::HELP.to_string())
        }
        [cmd, rest @ ..] if !cmd.starts_with('-') || cmd == "--window" => {
            if rest.iter().any(is_help) {
                // `files`/`ui` (the window's own requests) have none:
                // their usage error says what they take
                command_help(cmd).map_or(Help::None, Help::Text)
            } else {
                Help::None
            }
        }
        // flags only: --help among them is the whole help
        _ if args.iter().any(is_help) => Help::Text(usage_text()),
        _ => Help::None,
    }
}

/// The top-level flags given, when they are not one known mode: what is
/// wrong, in one line.
fn flags_problem(args: &[String], known: &[&str]) -> Option<String> {
    if let Some(a) = args.iter().find(|a| !known.contains(&a.as_str())) {
        let what = if a.starts_with('-') {
            "option"
        } else {
            "command"
        };
        return Some(format!(
            "hushmic: unknown {what} '{a}' (see hushmic --help)"
        ));
    }
    let mut modes: Vec<&str> = Vec::new();
    for a in args {
        if !modes.contains(&a.as_str()) {
            modes.push(a);
        }
    }
    (modes.len() > 1).then(|| {
        format!(
            "hushmic: {} can't be used together (one at a time)",
            modes.join(" and ")
        )
    })
}

/// `hushmic --window --help`: its options and the sample states.
fn window_help() -> String {
    let scenarios: Vec<&str> = hushmic::appstate::mock::Scenario::ALL
        .iter()
        .map(|s| s.word())
        .collect();
    // two-space indent, lines of at most 78
    let mut list = String::new();
    let mut line = String::from(" ");
    for w in scenarios {
        if line.len() + 1 + w.len() > 78 {
            list.push_str(&line);
            list.push('\n');
            line = String::from(" ");
        }
        line.push(' ');
        line.push_str(w);
    }
    list.push_str(&line);
    format!(
        "{}\n\nThe window alone: the running HushMic opens it, and closing it\n\
         leaves HushMic running. --mock shows a sample state instead (for\n\
         screenshots), one of:\n{list}\n",
        hushmic::window::usage(),
    )
}

/// The e2e seam: the Files page's workers fake their cleaning. Only a
/// build with the test-seams feature reads HUSHMIC_FAKE_FILE_WORKER.
#[cfg(feature = "test-seams")]
fn fake_file_worker() -> bool {
    std::env::var_os("HUSHMIC_FAKE_FILE_WORKER").is_some()
}

#[cfg(not(feature = "test-seams"))]
fn fake_file_worker() -> bool {
    false
}

fn main() {
    // A window or worker the daemon started is called "exe" otherwise.
    hushmic::selfexe::restore_name();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Started by "Open with": its output goes to a log it keeps small.
    if args == ["--tray"] && std::env::var_os(hushmic::files::cli::START_LOG_ENV).is_some() {
        hushmic::files::cli::keep_start_log_small();
    }
    // One file's cleaning pass, for the daemon.
    if args.len() > 1 && args[0] == "clean" && args[1] == "--worker" {
        std::process::exit(hushmic::files::worker::main(&args[2..]));
    }
    match help_request(&args) {
        Help::None => {}
        Help::Text(text) => {
            use std::io::Write;
            let _ = std::io::stdout().write_all(text.as_bytes());
            return;
        }
        Help::Unknown(msg) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    }
    // `-V` and `version` are --version.
    if matches!(args.as_slice(), [v] if v == "-V" || v == "version") {
        args = vec!["--version".into()];
    }
    // Clean recordings here and now, without the daemon.
    if args.first().is_some_and(|a| a == "clean") {
        std::process::exit(hushmic::files::cli::main(&args[1..]));
    }
    // "Open with HushMic": the files on the running instance's Files page.
    if args.first().is_some_and(|a| a == "open-files") {
        std::process::exit(hushmic::files::cli::open_files(&args[1..]));
    }
    // `watch` streams until the daemon goes away.
    if args.len() == 1 && args[0] == "watch" {
        let code = watch::cli_follow(&lock::default_control_socket_path(), &mut std::io::stdout());
        std::process::exit(code);
    }
    // Control subcommands are CLIENT invocations: talk to the running
    // tray's socket, print, exit. Same SIGPIPE stance as --version.
    if matches!(
        args.first().map(|s| s.as_str()),
        Some(
            "status"
                | "mode"
                | "toggle"
                | "unmute-to"
                | "quit"
                | "config"
                | "devices"
                | "service"
                | "watch"
                | "refresh"
                | "retry"
                | "shortcuts"
                | "miccheck"
                | "notice"
                | "profile"
                | "ui"
                | "files"
        )
    ) || (cfg!(feature = "speakers") && args.first().is_some_and(|s| s == "speakers"))
    {
        use std::io::Write;
        let (code, out) = cli_dispatch(&args);
        if out.is_empty() {
            std::process::exit(code);
        }
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
    // The v1 companion window: a child of the daemon (or a mock board for
    // building), with its own options.
    if args.first().map(String::as_str) == Some("--window") {
        let opts = match hushmic::window::parse_args(&args[1..]) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        // A child of the daemon (or standalone with --mock): no lock, no
        // signal plumbing; closing the window is the teardown.
        hushmic::window::set_panic_exits();
        let result = hushmic::window::run(opts, hushmic::pages::layer);
        let code = hushmic::window::exit_code(&result, hushmic::window::first_frame_shown());
        if let Err(e) = &result {
            if code == hushmic::window::EXIT_DISPLAY_GONE {
                eprintln!("[hushmic] the window's display went away: {e}");
            } else {
                eprintln!("hushmic: the window failed: {e}");
            }
            std::process::exit(code);
        }
        return;
    }
    let tray_mode = args.iter().any(|a| a == "--tray");
    let headless = args.iter().any(|a| a == "--headless");
    let enable_once = args.iter().any(|a| a == "--enable-once");
    let version = args.iter().any(|a| a == "--version");
    let doctor = args.iter().any(|a| a == "--doctor");
    const KNOWN_FLAGS: [&str; 5] = [
        "--tray",
        "--headless",
        "--enable-once",
        "--version",
        "--doctor",
    ];
    let modes = [tray_mode, headless, enable_once, version, doctor]
        .iter()
        .filter(|m| **m)
        .count();
    if let Some(msg) = flags_problem(&args, &KNOWN_FLAGS) {
        eprintln!("{msg}");
        std::process::exit(1);
    }
    // No flag at all is the DESKTOP LAUNCH: run the tray AND open the
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
        std::process::exit(hushmic::diagnostics::exit_code(problems));
    }

    let mode = if headless {
        "headless"
    } else if enable_once {
        "enable-once"
    } else {
        "tray"
    };
    let _lock = acquire_single_instance(show_mode, mode);
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
    // What an earlier version generated next to the config moves to the
    // state folder first: the recovery below reads the breadcrumb.
    controller::migrate_state();
    controller::recover_dangling_default();
    // Engine meter files and tap requests a previous run left behind (the
    // chain was killed with it): no chain of ours runs yet.
    if let Some(d) = hushmic::meter::dir() {
        hushmic::meter::sweep(&d);
    }
    // Mic check's replay file from a window that crashed: the user's
    // voice must not outlive the window.
    hushmic::miccheck::remove_files();

    if enable_once {
        // Scripted/integration use: enable, then hold until terminated. The
        // Drop teardown restores the previous default and reaps the child.
        let mut c = Controller::new(Paths::resolve());
        let cfg = Config::load();
        if let Err(e) = c.enable(&cfg) {
            eprintln!("hushmic: enable failed: {e}");
            std::process::exit(1);
        }
        // enable() succeeding only means the child SPAWNED. Headless there is
        // no watchdog or notification to catch a node that never registers
        // (e.g. PipeWire not running), so verify before claiming success —
        // otherwise scripts hang on a mic that will never exist. The wait
        // returns as soon as the chain is linked; the full wait (the link
        // job's own limit and a second) is only the failure path.
        if !c.wait_linked(hushmic::link::LINK_LIMIT + Duration::from_secs(1)) {
            eprintln!(
                "hushmic: enable failed: the chain never reached hushmic_source (is PipeWire \
                 running?)"
            );
            // Explicit drop, not bare exit: Drop reaps the child and
            // restores/clears the default source we may have taken.
            drop(c);
            std::process::exit(1);
        }
        // A node slower than enable()'s bounded wait (a busy machine)
        // leaves the default untouched; the daemon's tick finishes that
        // takeover, and this mode has no tick: finish it here, waiting for
        // the attempt (no loop to hold up).
        for _ in 0..3 {
            c.ensure_default_takeover(&cfg, true);
            c.finish_takeover(hushmic::takeover::ATTEMPT_CAP + Duration::from_secs(1));
            if !cfg.set_default || c.default_landed() {
                break;
            }
        }
        eprintln!("hushmic: enabled; send SIGTERM or press Ctrl+C to stop.");
        wait_for_shutdown(shutdown_pipe);
        drop(c);
        return;
    }

    // Before anything could write them: a fresh install starts first run.
    let config_existed = windowstate::ran_before(
        &Config::path(),
        &controller::conf_path(),
        &autostart::desktop_path(),
    );
    let mut cfg = Config::load();
    // Every write of it from here on: off the loop, which also answers
    // push to talk, and only when something stored changed.
    let saver = hushmic::config::Saver::spawn();
    notify::set_enabled(cfg.notifications);
    let mut controller = Controller::new(Paths::resolve());
    // The speakers cleaner: a chain of its own, started and stopped with
    // its switch and with HushMic (see hushmic::speakers).
    #[cfg(feature = "speakers")]
    let mut speakers = hushmic::speakers::Speakers::new(Paths::resolve());
    // The settings it was last brought to (None: not yet).
    #[cfg(feature = "speakers")]
    let mut speakers_applied: Option<hushmic::speakers::Settings> = None;
    // The hold's last scheduled expiry check.
    #[cfg(feature = "speakers")]
    let mut hold_wake: Option<Instant> = None;
    // The speakers cleaner's last asked-for look.
    #[cfg(feature = "speakers")]
    let mut speakers_wake: Option<Instant> = None;
    // For `config set model` validation: the same directory enable() reads.
    let model_dir = Paths::resolve().model_dir;

    // A previous run that died mid-mic-test never got to delete its
    // recordings — the user's voice must not linger on disk.
    mictest::remove_recordings();

    let (tx, rx) = mpsc::channel::<Event>();
    controller.on_port_failure({
        let tx = tx.clone();
        Box::new(move |generation| {
            let _ = tx.send(Event::PortWriteFailed(generation));
        })
    });
    controller.on_takeover_done({
        let tx = tx.clone();
        Box::new(move || {
            let _ = tx.send(Event::TakeoverDone);
        })
    });
    controller.on_link_done({
        let tx = tx.clone();
        Box::new(move |done| {
            let _ = tx.send(Event::Linked(done));
        })
    });
    // The chain whose port write did not land and still needs a look:
    // written again at a tick, or left to its control file once its plugin
    // maps it (portwriter::missed).
    let mut ports_owed: Option<u64> = None;
    // The graph probe out since then (see start_graph_probe), and whether
    // its slowness was logged.
    let mut graph_probe: Option<Instant> = None;
    // Another probe is wanted as soon as the one out comes back (it may
    // predate what asked for it).
    let mut graph_again = false;
    // A device plugged in or out is read at once, not at the next tick.
    pipewire::watch_graph({
        let tx = tx.clone();
        move || {
            let _ = tx.send(Event::Look);
        }
    });
    // When a look is asked for a pending microphone switch.
    let mut switch_look: Option<Instant> = None;
    // When a new run last asked for a probe.
    let mut run_probe_at: Option<Instant> = None;
    // A tick asked for a look since the last snapshot was judged. The
    // switch policies (mic recovery, default release, settings follow)
    // count ticks: fed every snapshot, they would count every probe a
    // new run or a command asks for too, and their debounce and cooldown
    // would pass in a few seconds instead of 10 and 30.
    let mut tick_owed = false;
    let mut graph_slow_logged = false;
    // The last graph read failed or timed out.
    let mut graph_unanswered = false;

    // Tray -> commands
    let (ctx, crx) = mpsc::channel::<TrayCmd>();
    let mut known_mics = pipewire::pw_dump_within(pipewire::CALL_BUDGET)
        .map(|d| pipewire::filter_real(&pipewire::parse_pwdump_nodes(&d)))
        .unwrap_or_default();
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
        profile: cfg.profile_for(None),
        mics: mics.to_vec(),
        cmd_tx: ctx.clone(),
        status: TrayStatus::Off,
        mode: RunMode::default(),
        engine: None,
        engine_light_configured: false,
        icon_style: hushmic::tray::icon_style_for(cfg),
        extras: TrayExtras::default(),
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
    // The window's watch channel: the listener hands `watch` connections
    // to the hub, the loop publishes a snapshot after every event.
    let hub = watch::Hub::new();
    if let Some(listener) = control_listener {
        let (ctl_tx, ctl_rx) = mpsc::channel::<control::ControlReq>();
        control::spawn_listener_with(listener, ctl_tx, Some(hub.clone()));
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
    // The one-time notice for the user's first close of the window, and
    // the desktop's word on a logout under way (which it must not spend).
    let mut first_close = notify::FirstClose::default();
    // What the window was last shown, for "running again" after a reconnect.
    let mut shown = Shown::default();
    if !headless {
        hushmic::session_end::spawn();
    }

    let saved_ui = windowstate::load();
    let mut watched = Watched {
        guard: pipewire::IntruderGuard::default(),
        lifecycle: Lifecycle::since(hushmic::diagnostics::engine_generation()),
        start_why: Start::Launch,
        slots: HashMap::new(),
        system_default: None,
        feeder: None,
        unlinked_since: None,
        using: Vec::new(),
        users: Vec::new(),
        off_moved: Vec::new(),
        return_from: None,
        return_asked: false,
        direct: Vec::new(),
        direct_watch: apps::DirectWatch::default(),
        names: apps::Names::from_env(),
        released_to: None,
        shortcuts: ShortcutsState {
            keys: ShortcutsState::unreported(),
            ..ShortcutsState::default()
        },
        pw_failures: 0,
        graph_at: None,
        ui: windowstate::at_start(saved_ui.clone(), config_existed),
        ui_saved: saved_ui.clone(),
        desktop: hushmic::desktop::Look::default(),
        runs: {
            let tx = tx.clone();
            hushmic::usemark::Watch::spawn(Box::new(move || {
                let _ = tx.send(Event::RunBegan);
            }))
        },
        use_mark: Default::default(),
        ask: None,
    };
    // The desktop's light/dark style and reduced-motion wish, read for the
    // window (it opens no bus of its own); started with the first window
    // (see window_look).
    let mut desktop: Option<hushmic::desktop::Reader> = None;
    // A fresh install's state is written at once: a config saved before
    // first run ends must not turn it into an upgrade at the next start.
    watched.save_ui();
    // The engine tier changes on the tee thread: wake the loop so the
    // window hears it now, not at the next tick.
    {
        let tx = tx.clone();
        hushmic::diagnostics::on_engine_change(move || {
            let _ = tx.send(Event::Wake);
        });
    }
    // A problem held for Live's Status card past its grace: judge it now,
    // not at the next tick.
    {
        let tx = tx.clone();
        notify::set_wake(move || {
            let _ = tx.send(Event::Wake);
        });
    }

    // The Files page's jobs: what a crash or power cut left in the cache
    // goes first.
    let files_root = hushmic::files::cache::root_or_temp();
    hushmic::files::jobs::sweep(&files_root);
    let files_base = hushmic::files::jobs::EngineChoice {
        model: String::new(),
        inference: hushmic_denoiser::Inference::Auto,
        ffmpeg: hushmic::files::decode::which("ffmpeg"),
        fake: fake_file_worker(),
    };
    let mut jobs = {
        let tx = tx.clone();
        // The binary that runs, also after an upgrade replaced its file.
        let workers = hushmic::files::jobs::Processes::this_binary(move |ev| {
            let _ = tx.send(Event::Files(ev));
        });
        let engine = files_engine(&cfg, &files_base);
        let mut jobs = hushmic::files::jobs::Jobs::new(files_root, engine, Box::new(workers));
        // The page's strength starts where the user left it, else at
        // Strong (jobs::DEFAULT_DB), not the Settings' (Live's): a file
        // keeps a little of its room.
        if let Some(db) = cfg.files_strength {
            jobs.set_strength(db);
        }
        jobs.set_folder(hushmic::sandbox::is_flatpak(), cfg.files_folder.clone());
        jobs
    };
    // "Show" on "3 recordings cleaned.": the window, on Files; "Show in
    // folder" (every one saved, in one folder): that folder.
    if !headless {
        let tx = tx.clone();
        notify::on_action(move |slot, key| {
            // "Turn on" on "HushMic is off", "Use Built-in Audio" on "No
            // sound from …" (UX panel round 2, A5)
            if let Some(cmd) = notice_command(slot, &key) {
                let _ = tx.send(Event::Cmd(cmd));
                return;
            }
            if slot != notify::Slot::Files {
                return;
            }
            match key.as_str() {
                // "Save all" on "2 recordings are ready.": saved where the
                // page's Save all would
                "save-all" => {
                    let _ = tx.send(Event::FilesSaveAll);
                }
                "folder" if hushmic::files::note::show_folder() => {}
                _ => {
                    let _ = tx.send(Event::Cmd(TrayCmd::OpenFiles));
                }
            }
        });
    }

    // The v1 window (see open_companion).
    let mut companion: Option<Companion> = None;
    // Launch B: a plain launch opens the window at once and the window
    // follows the start over the watch channel (Starting, then Ready),
    // instead of waiting for the microphone first.
    if show_mode {
        if cfg.enabled {
            watched.lifecycle.begin(Start::Launch, Instant::now());
        }
        // The first snapshot and the window's first frame in the desktop's
        // style.
        let look = window_look(&mut desktop, &tx, &mut watched, &cfg, &[]);
        hub.publish(&app_state(
            &watched,
            &cfg,
            &mut controller,
            &known_mics,
            None,
            &handle,
            headless,
            RunMode::Suppress,
            &mut None,
            false,
            false,
            false,
            &gate,
        ));
        open_companion(
            &mut companion,
            windowstate::fresh_page(&watched.ui, false),
            watched.ui.size,
            look,
            &[],
            &tx,
        );
    }

    // apply persisted state on launch
    let _ = autostart::reconcile(cfg.autostart);
    if cfg.enabled {
        match controller.enable(&cfg) {
            Ok(()) => schedule_early_tick(&tx),
            Err(e) => {
                eprintln!("hushmic: enable failed: {e}");
                watched
                    .lifecycle
                    .failed(&e.to_string(), hushmic::diagnostics::engine_generation());
                // A launch failure is invisible on stderr for
                // .desktop/autostart starts — surface it (launch counts as
                // user-initiated).
                if gate.on_enable_error(&e.to_string(), true) {
                    notify::send_problem(
                        notify::Problem::Failure,
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
        let dump = pipewire::pw_dump_within(pipewire::CALL_BUDGET);
        match dump.as_deref() {
            Some(d) => watched.observe_graph(d, &mut controller),
            None => watched.pw_failed(&tx),
        }
        let present = dump.as_deref().map(|d| pipewire::chain_graph(d).up());
        let status = compute_status(&cfg, &mut controller, present);
        let profile = cfg.profile_for(settings_device(&cfg, &controller, &mut None).as_deref());
        let _ = handle.update(move |t: &mut HushMicTray| {
            t.status = status;
            t.profile = profile;
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
    // set by `hushmic miccheck`, cleared on Event::MicTestDone. The flag lets
    // the loop cancel the test when the filter-chain is mutated under it.
    let mut testing = false;
    // How the last mic check ended (`miccheck state`): done, cancelled,
    // or failed with its message.
    let mut mictest_end = String::from("done");
    let mut mictest_cancel: Option<Arc<AtomicBool>> = None;
    // The toggle overlay's return address: the last chain-alive mode before
    // the current one, updated on EVERY SetMode (tray radio or CLI) so
    // tray-mute then CLI-untoggle round-trips. See control::update_prev_alive.
    let mut prev_alive = RunMode::Suppress;
    // `unmute-to` while not muted: what the next mute returns to
    let mut unmute_stored: Option<RunMode> = None;
    // Last probe verdict on "the chain feeds hushmic_source" (both hosts'
    // nodes and the link), refreshed by Tick and the Cmd epilogue —
    // `status` reports it instead of re-probing on the hot path.
    let mut last_node_present: Option<bool> = None;
    // The chain generation whose link failure was logged (once per chain:
    // the tick asks again every 5 s while it persists).
    let mut link_failure_logged = 0u64;
    // PipeWire's scheduling state for the chain's capture from the same
    // snapshots (`hushmic status`: idle / running).
    let mut last_node_state: Option<String> = None;
    // The chain generation that state was read under.
    let mut node_state_gen = 0u64;
    let mut tray_engine = EngineCache::default();
    // The tray's status line and device names as last landed (None: not
    // yet, or a tray that registered since).
    let mut tray_extras: Option<TrayExtras> = None;
    // Metadata stream routing (the re-pin write) exists only on modern
    // PipeWire; probed once — on legacy hosts the theft mechanism does not
    // exist either, and inert writes would just spam the log.
    let repin_supported = pipewire::supports_target_object();
    // The capture healer's memory: corrections since the stream last held,
    // and whether the tug-of-war notice went out — see pipewire::Repin.
    let mut repin = pipewire::Repin::default();
    // The look the healer owes, asked for (see Event::HealerLook).
    let mut healer_look: Option<Instant> = None;
    // The microphone sends exact digital silence to an app (UX panel A4),
    // looked at on the tick.
    let mut silence = hushmic::meter::SilenceWatch::default();
    let mut mic_silent = false;
    // Settings follow the default source (see the Tick arm).
    let mut profile_follow = watchdog::ProfileFollow::new();
    // The device the chain followed as the system default at the last
    // tick (None: not following), to tell a switch.
    let mut followed_seen: Option<String> = None;
    let mut default_release = watchdog::DefaultRelease::default();
    // The device a start would take its settings from, as last seen with
    // no chain running, and the `mic` it was resolved for — spares the
    // main loop a pw-dump per status/config request (see settings_device;
    // `status` only re-reads the node state, bounded, while it reads idle).
    let mut target_cache: TargetCache = None;
    // Shortcuts portal state: None until the worker's first report;
    // Some(false) makes Tick nudge a reconnect, throttled by the backoff.
    let mut shortcuts_up: Option<bool> = None;
    let mut shortcuts_backoff = watchdog::Backoff::new();
    // What the hold keys are doing (push-to-mute's armed release,
    // push-to-talk's dangling-hold re-mute) — see shortcuts::Holds.
    let mut shortcut_holds = shortcuts::Holds::default();

    loop {
        // The speakers cleaner follows every settings change (its switch,
        // output and strength, HushMic on or off) the last event made.
        #[cfg(feature = "speakers")]
        let wanted = hushmic::speakers::Settings::of(&cfg);
        #[cfg(feature = "speakers")]
        if speakers_applied.as_ref() != Some(&wanted) {
            speakers.apply(&cfg);
            speakers_applied = Some(wanted);
        }
        // A retry, the streams of a restarted chain, a re-target to check:
        // a look then, not at the next tick (up to 5 s away).
        #[cfg(feature = "speakers")]
        if let Some(at) = speakers.wake_at() {
            if speakers_wake != Some(at) {
                speakers_wake = Some(at);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(at.saturating_duration_since(Instant::now()));
                    let _ = tx.send(Event::Look);
                });
            }
        }
        // Before waiting for the next event: notice a chain (re)spawn the
        // last one caused, and show the window what is true now.
        let generation = hushmic::diagnostics::engine_generation();
        // A (re)spawn's "up" comes with its link (Event::Linked).
        watched.lifecycle.note(
            generation,
            controller.is_running(),
            &mut watched.start_why,
            Instant::now(),
        );
        controller.note_mics(&known_mics);
        watched.runs.chain(controller.is_running());
        for (i, m) in known_mics.iter().enumerate() {
            watched.slots.insert(m.name.clone(), i);
        }
        let mut state = app_state(
            &watched,
            &cfg,
            &mut controller,
            &known_mics,
            last_node_present,
            &handle,
            headless,
            prev_alive,
            &mut target_cache,
            repin.rerouted(),
            repin.gave_up(),
            mic_silent,
            &gate,
        );
        // Back after a restart: the window showed "Reconnecting…", so it
        // says "Running again." (no desktop notification for a routine
        // reconnect), unless this outage's recovery was already counted.
        if shown.back_after_restart(state.engine.phase, gate.recoveries()) && gate.on_reconnected()
        {
            state.engine.recovered = gate.recoveries();
        }
        // Cleaning files waits while an app uses HushMic and the
        // microphone's engine is stepped down for want of CPU.
        jobs.set_engine(files_engine(&cfg, &files_base));
        let stepped_down = match state.engine.tier {
            Some(hushmic::diagnostics::EngineTier::Passthrough) => true,
            Some(hushmic::diagnostics::EngineTier::Light) => !state.engine.configured_light,
            _ => false,
        };
        // So does a stepped-down speakers chain while something plays
        // through it: the microphone first, then speakers, then files.
        #[cfg(feature = "speakers")]
        let speakers_stepped = speakers.stepped_down();
        #[cfg(not(feature = "speakers"))]
        let speakers_stepped = false;
        jobs.set_live(
            stepped_down || speakers_stepped,
            state.in_use() || speakers_stepped,
        );
        // A chosen folder that went away is dropped before the window asks
        // for a Save (it then asks for one again): looked at while the
        // window shows Files, never while it is closed.
        let files_shown = companion.is_some() && watched.ui.page == hushmic::appstate::Page::Files;
        jobs.check_folder(files_shown);
        jobs.tick();
        // "Save all and quit": every file saved, so quit
        if jobs.quit_ready() {
            let _ = tx.send(Event::Cmd(TrayCmd::Quit));
        }
        state.files = jobs.state();
        state.ask = watched.ask;
        // A hold not renewed in time ends (its own wake below).
        #[cfg(feature = "speakers")]
        speakers.expire_hold(Instant::now());
        #[cfg(feature = "speakers")]
        if let Some(at) = speakers.hold_until() {
            if hold_wake != Some(at) {
                hold_wake = Some(at);
                let tx = tx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(at.saturating_duration_since(Instant::now()));
                    let _ = tx.send(Event::Wake);
                });
            }
        }
        #[cfg(feature = "speakers")]
        {
            state.speakers = speakers.app_state(&cfg, &watched.names);
        }
        // Files cleaned while the window was closed or in the background:
        // one note for them all.
        if let Some(c) = jobs.take_cleaned() {
            if !companion.as_ref().is_some_and(|c| c.focused) {
                hushmic::files::note::send(&c);
            }
        }
        // the note's Save all, through: "Saved 2 recordings."
        if let Some(c) = jobs.take_saved() {
            if !companion.as_ref().is_some_and(|c| c.focused) {
                hushmic::files::note::send(&c);
            }
        }
        // The tray's status line reads the same snapshot.
        let extras = TrayExtras::of(&state);
        if tray_extras.as_ref() != Some(&extras) {
            let push = extras.clone();
            let landed = handle
                .update(move |t: &mut HushMicTray| t.extras = push)
                .is_some();
            tray_extras = landed.then_some(extras);
        }
        hub.publish(&state);
        release_held(&state);
        let Ok(ev) = rx.recv() else { break };
        // Mutating control requests become synthetic SetMode commands so
        // they share the entire Cmd path (mic-test invalidation, live
        // switch + restart fallback, config save, tray refresh); the reply
        // is written from the Cmd epilogue once the outcome is known.
        // Read-only requests answer inline from loop-owned state.
        let mut control_reply: Option<mpsc::Sender<String>> = None;
        // the mode change below is push-to-mute's press (its mute is a
        // hold: control::after_transition)
        let mut mute_hold = false;
        let ev = match ev {
            Event::Control(req) => {
                // A request whose client stopped waiting (it was told
                // "did not answer in time", or hung up) changes nothing.
                if !req.claim() {
                    eprintln!(
                        "[hushmic] dropped \"{}\": its client stopped waiting",
                        req.line
                    );
                    continue;
                }
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
                    Ok(control::Request::UnmuteTo(m)) => {
                        // The window's switch while muted: it picks what
                        // unmuting (and the toggles, push-to-talk) return
                        // to, and the mic stays muted.
                        // Not muted: kept for the next mute (UX panel
                        // round 2, A11).
                        let cur = cfg.enabled.then(|| controller.mode());
                        let msg = if cur == Some(RunMode::Mute) {
                            prev_alive = m;
                            control::encode_ok(&format!(
                                "muted; unmuting returns to {}",
                                control::mode_word(Some(m))
                            ))
                        } else {
                            unmute_stored = Some(m);
                            control::encode_ok(&format!(
                                "not muted (the mode is {}); after the next mute, unmuting \
                                 returns to {}",
                                control::mode_word(cur),
                                control::mode_word(Some(m))
                            ))
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    Ok(control::Request::Status { json }) => {
                        let profile = cfg.profile_for(
                            settings_device(&cfg, &controller, &mut target_cache).as_deref(),
                        );
                        let saved = profile.device.clone().filter(|_| profile.saved);
                        let engine = controller
                            .is_running()
                            .then(hushmic::diagnostics::engine_tier)
                            .flatten();
                        let configured_light = controller.active_model_is_light();
                        let inference = controller
                            .active_model()
                            .and_then(|m| control::live_model(engine, m, configured_light))
                            .and_then(|m| hushmic::diagnostics::inference_for(&m));
                        // The tick's snapshot is up to 5 s old. A running
                        // chain stays running for the call, so that reading
                        // is used as is; an idle or unknown one is re-read,
                        // because idle flips the moment a call starts and a
                        // status taken right after joining must not say
                        // idle. Bounded: a wedged daemon must not stall the
                        // loop (a failed probe reads as unknown), and not at
                        // all while the tick's graph read is still out (the
                        // session manager is not answering).
                        let node_state = if !controller.is_running() {
                            None
                        } else {
                            match last_node_state.as_deref() {
                                Some(st) if !pipewire::state_is_idle(st) => last_node_state.clone(),
                                _ if graph_probe.is_some() => last_node_state.clone(),
                                _ => pipewire::pw_dump_within(STATUS_PROBE_BUDGET)
                                    .and_then(|d| pipewire::node_state(&d, pipewire::CHAIN_INPUT)),
                            }
                        };
                        let linked = linked_mic(&watched, &mut controller);
                        let s = control::Status {
                            version: env!("CARGO_PKG_VERSION").to_string(),
                            mode: cfg.enabled.then(|| controller.mode()),
                            mic_configured: cfg.mic.clone(),
                            mic_active: linked.clone(),
                            // by name, as the window says them (UX panel
                            // round 2, A11); --json keeps the node names
                            mic_names: [cfg.mic.as_deref(), linked.as_deref()].map(|d| {
                                d.map(|d| mic_label(&cfg, &controller, &known_mics, None, Some(d)))
                            }),
                            fallback_active: on_fallback(
                                &cfg,
                                &mut controller,
                                &known_mics,
                                linked.as_deref(),
                            ),
                            model: profile.model.clone(),
                            attn_limit: profile.attn_limit,
                            profile_name: saved
                                .as_deref()
                                .and_then(|d| controller.mic_name(&cfg, &known_mics, d)),
                            profile: saved,
                            defaults_model: cfg.model.clone(),
                            defaults_attn_limit: cfg.attn_limit,
                            chain_running: controller.is_running(),
                            linked: watched.graph_at.map(|_| {
                                watched
                                    .unlinked_since
                                    .is_none_or(|t| t.elapsed() < control::NOT_LINKED_AFTER)
                            }),
                            node_present: last_node_present,
                            node_state,
                            control_file: controller.control_file(),
                            tray: tray_host(&handle),
                            engine,
                            configured_light,
                            inference,
                            apps_using: watched.using.clone(),
                            apps_direct: watched.direct.clone(),
                            apps_kept_off: watched.guard.kept_off(),
                            #[cfg(feature = "speakers")]
                            speakers: speakers.status(&cfg),
                        };
                        let payload = if json {
                            control::render_status_json(&s)
                        } else {
                            control::render_status_human(&s)
                        };
                        let _ = req.reply.send(control::encode_ok(&payload));
                        continue;
                    }
                    Ok(control::Request::Quit { force: false })
                        if jobs.quit_refusal().is_some() =>
                    {
                        let why = jobs.quit_refusal().unwrap_or_default();
                        let _ = req.reply.send(control::encode_err(&why));
                        continue;
                    }
                    Ok(control::Request::Quit { .. }) => {
                        // Answer first, and wait until the bytes are on the
                        // wire: the Quit arm breaks the loop and the process
                        // exits right after — with the chain already off,
                        // that is microseconds away.
                        let _ = req.reply.send(control::encode_ok("stopping"));
                        let _ = req.written.recv_timeout(Duration::from_secs(2));
                        Event::Cmd(TrayCmd::Quit)
                    }
                    Ok(control::Request::ConfigGet { key, json }) => {
                        let device = settings_device(&cfg, &controller, &mut target_cache);
                        let msg = match hushmic::config_cli::offline_get(
                            &cfg,
                            key.as_deref(),
                            json,
                            device.as_deref(),
                        ) {
                            Ok(s) => control::encode_ok(&s),
                            Err(e) => control::encode_err(&e),
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    Ok(control::Request::ConfigSet {
                        key,
                        value,
                        written,
                        defaults,
                    }) => {
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
                        // model/attn_limit land where the tray's edits do:
                        // the device in use (see Config::set_model_for).
                        let settled = matches!(k, cc::Key::Model | cc::Key::AttnLimit)
                            && controller.settle_in_use(&cfg);
                        // (`--defaults`: the defaults, every microphone
                        // without its own)
                        let device = settings_device(&cfg, &controller, &mut target_cache)
                            .filter(|_| !defaults);
                        let mut new = cfg.clone();
                        cc::apply(&mut new, k, v, device.as_deref());
                        // A mic pick (even of the same mic) ends a recovery
                        // fallback, like the tray's: it restarts the chain.
                        let fallback_ended = (k == cc::Key::Mic || new.mic != cfg.mic)
                            && controller.end_mic_fallback();
                        let chain_changed = hushmic::controller::settings_restart(
                            cc::diff(&cfg, &new).chain,
                            fallback_ended,
                        );
                        if k == cc::Key::SetDefault {
                            // Deliberate: may take the default again even
                            // after the user switched it away.
                            watched.released_to = None;
                            controller.reclaim_default();
                        }
                        // Resolved for the chain as it was; the change
                        // may move or stop it.
                        target_cache = None;
                        let res = apply_config(
                            &mut cfg,
                            new,
                            &mut controller,
                            &apply,
                            testing,
                            &mictest_cancel,
                            &mut gate,
                            fallback_ended,
                        );
                        // the chain still carries the settings of the
                        // device it left: bring it to the one in use
                        let res = match res {
                            Ok(()) if settled && cfg.enabled => {
                                apply_mic(&mut controller, &cfg, testing, &mictest_cancel)
                            }
                            r => r,
                        };
                        if k == cc::Key::SetDefault && res.is_ok() && cfg.enabled && !chain_changed
                        {
                            // Asked again with the setting already on (Live's
                            // "Make it the default again"): nothing in the
                            // chain changes, so take it now, not at the next
                            // tick. After a restart the node seen last is the
                            // old chain's: the tick takes it once the new one
                            // is up.
                            controller
                                .ensure_default_takeover(&cfg, last_node_present == Some(true));
                        }
                        // A speakers setting reaches its chain now, so the
                        // answer can say when it could not start.
                        #[cfg(feature = "speakers")]
                        let res = if matches!(
                            k,
                            cc::Key::SpeakersEnabled
                                | cc::Key::SpeakersOutput
                                | cc::Key::SpeakersAttn
                        ) {
                            speakers.apply(&cfg);
                            speakers_applied = Some(hushmic::speakers::Settings::of(&cfg));
                            match (res, speakers.last_error()) {
                                (Ok(()), Some(e)) if hushmic::speakers::wanted_on(&cfg) => {
                                    Err(format!("HushMic Speakers could not start: {e}"))
                                }
                                (r, _) => r,
                            }
                        } else {
                            res
                        };
                        let echo = cc::set_echo(&cfg, k, device.as_deref(), defaults);
                        let mut line = cc::fold(&echo, cc::set_qualifier(k, true));
                        if matches!(k, cc::Key::Tray | cc::Key::TrayIcon) && headless {
                            line = cc::fold(&line, " (--headless ignores it)");
                        }
                        if k == cc::Key::Autostart && hushmic::sandbox::is_flatpak() {
                            line = cc::fold(&line, " (requested; the desktop decides)");
                        }
                        let reply = req.reply.clone();
                        let answer = move |res: Result<(), String>| {
                            let _ = reply.send(match res {
                                Ok(()) => control::encode_ok(&line),
                                Err(e) => control::encode_err(&format!("{line}; {e}")),
                            });
                        };
                        if written {
                            // the command line: answered once the setting
                            // is on disk too, so a failed write is reported
                            saver.save_then(&cfg, move |w| answer(with_write(res, w)));
                        } else {
                            // the window: answered once applied, so its
                            // next action (a Mute) never waits behind a
                            // slow disk; a failed write comes as a notice
                            answer(res);
                            save_noting(&saver, &cfg, &tx);
                        }
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
                            &known_mics,
                            last_node_present,
                            &mut tray_engine,
                            &mut target_cache,
                        );
                        start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
                        continue;
                    }
                    Ok(control::Request::Watch) => {
                        // The listener hands these to the hub; one that
                        // gets here came from a listener without one.
                        let _ = req
                            .reply
                            .send(control::encode_err("watch is not available"));
                        continue;
                    }
                    #[cfg(feature = "speakers")]
                    Ok(control::Request::Speakers) => {
                        let st = speakers.status(&cfg);
                        let out = hushmic::speakers::render_show(&cfg, Some(&st));
                        let _ = req.reply.send(control::encode_ok(&out));
                        continue;
                    }
                    #[cfg(feature = "speakers")]
                    Ok(control::Request::SpeakersHold(on)) => {
                        let msg = if !on {
                            speakers.set_bypass(false);
                            control::encode_ok("speakers hold off")
                        } else if speakers.set_bypass(true) {
                            control::encode_ok("speakers hold on")
                        } else {
                            control::encode_err("HushMic Speakers is not running")
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    #[cfg(feature = "speakers")]
                    Ok(control::Request::SpeakersRetry) => {
                        let msg = if !hushmic::speakers::wanted_on(&cfg) {
                            control::encode_ok("HushMic Speakers is off")
                        } else if speakers.retry(&cfg) {
                            match speakers.last_error() {
                                Some(e) => control::encode_err(&format!(
                                    "HushMic Speakers could not start: {e}"
                                )),
                                None => control::encode_ok("starting HushMic Speakers"),
                            }
                        } else {
                            control::encode_ok("HushMic Speakers is running")
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    Ok(control::Request::Refresh) => {
                        // A graph read under a second ago is fresh enough:
                        // a window asking every frame must not keep the loop
                        // (and push-to-talk) waiting on pw-dump.
                        if watched
                            .graph_at
                            .is_some_and(|t| t.elapsed() < REFRESH_FRESH)
                        {
                            let _ = req.reply.send(control::encode_ok("refreshed"));
                            continue;
                        }
                        // Bounded: the window asks for this while it waits
                        // (first run), and the loop must stay responsive;
                        // with the tick's graph read still out the session
                        // manager is not answering, and waiting is pointless.
                        let probe = if graph_probe.is_some() {
                            pipewire::DumpProbe::TimedOut
                        } else {
                            pipewire::pw_dump_probe(STATUS_PROBE_BUDGET)
                        };
                        let msg = match probe {
                            pipewire::DumpProbe::Dump(d) => {
                                let nodes = pipewire::parse_pwdump_nodes(&d);
                                last_node_present = Some(pipewire::chain_graph(&d).up());
                                last_node_state = pipewire::node_state(&d, pipewire::CHAIN_INPUT);
                                node_state_gen = hushmic::diagnostics::engine_generation();
                                watched.lifecycle.observe(
                                    hushmic::diagnostics::engine_generation(),
                                    last_node_present,
                                );
                                let real = pipewire::filter_real(&nodes);
                                if real != known_mics {
                                    known_mics = real.clone();
                                    let _ = handle.update(move |t: &mut HushMicTray| {
                                        t.mics = real;
                                    });
                                }
                                watched.observe_graph(&d, &mut controller);
                                control::encode_ok("refreshed")
                            }
                            // Slow is not gone: only a failed probe counts
                            // toward "can't reach PipeWire".
                            pipewire::DumpProbe::TimedOut => {
                                control::encode_err("PipeWire did not answer in time")
                            }
                            pipewire::DumpProbe::Failed => {
                                watched.pw_failed(&tx);
                                control::encode_err("PipeWire did not answer")
                            }
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    // The answer to the tray's Quit: stop the files and quit,
                    // the reply on the wire first (as for `quit`).
                    Ok(control::Request::Files(control::FilesRequest::QuitStop)) => {
                        let _ = req.reply.send(control::encode_ok("stopping"));
                        let _ = req.written.recv_timeout(Duration::from_secs(2));
                        Event::Cmd(TrayCmd::Quit)
                    }
                    // Quit in the window: as the tray's Quit, but the window
                    // is up, so the question needs no window opened.
                    Ok(control::Request::Files(control::FilesRequest::QuitAsk)) => {
                        if tray_quit(jobs.at_stake(), jobs.asking_quit()) == TrayQuit::Ask {
                            jobs.set_ask_quit(true);
                            let _ = req.reply.send(control::encode_ok("asking"));
                            continue;
                        }
                        let _ = req.reply.send(control::encode_ok("stopping"));
                        let _ = req.written.recv_timeout(Duration::from_secs(2));
                        Event::Cmd(TrayCmd::Quit)
                    }
                    // "Open with": the window on Files, its answer first.
                    Ok(control::Request::Files(control::FilesRequest::Open)) => {
                        let _ = req.reply.send(control::encode_ok("ok"));
                        Event::Cmd(TrayCmd::OpenFiles)
                    }
                    Ok(control::Request::Files(r)) => {
                        use control::FilesRequest as F;
                        let known = |ok: bool| {
                            if ok {
                                control::encode_ok("ok")
                            } else {
                                control::encode_err("no such file")
                            }
                        };
                        let msg = match r {
                            F::Add(path) => {
                                let path = control::files_add_path(&req.line).unwrap_or(path);
                                match jobs.add(&path) {
                                    Ok(id) => control::encode_ok(&id.to_string()),
                                    Err(e) => control::encode_err(&e),
                                }
                            }
                            F::Remove(id) => known(jobs.remove(id)),
                            F::Play { id, at } => known(jobs.play(id, at)),
                            F::Retry(id) => known(jobs.retry(id)),
                            F::Strength { db, id } => {
                                let known_file = match id {
                                    Some(id) => jobs.set_file_strength(id, db),
                                    None => {
                                        jobs.set_strength(db);
                                        true
                                    }
                                };
                                // remembered: the next start's page strength
                                // (not for a file that isn't there)
                                if known_file && cfg.files_strength != Some(db) {
                                    cfg.files_strength = Some(db);
                                    saver.save(&cfg);
                                }
                                known(known_file)
                            }
                            F::Track { id, track } => match jobs.set_track(id, track) {
                                Ok(()) => control::encode_ok("ok"),
                                Err(e) => control::encode_err(&e),
                            },
                            F::CleanAnyway => {
                                jobs.clean_anyway();
                                control::encode_ok("ok")
                            }
                            F::CleanNext(id) => known(jobs.clean_next(id)),
                            F::Save { id, dir } => {
                                let dir =
                                    dir.map(|d| control::files_save_dir(&req.line).unwrap_or(d));
                                match jobs.save(id, dir.map(std::path::PathBuf::from)) {
                                    Ok(()) => control::encode_ok("ok"),
                                    Err(e) => control::encode_err(&e.to_string()),
                                }
                            }
                            F::CancelSave(id) => known(jobs.cancel_save(id)),
                            F::Replace { id, path } => {
                                let path = control::files_replace_path(&req.line).unwrap_or(path);
                                match jobs.replace(id, &path) {
                                    Ok(()) => control::encode_ok("ok"),
                                    Err(e) => control::encode_err(&e),
                                }
                            }
                            F::CleanFirst(id) => known(jobs.clean_first(id)),
                            F::Clear => control::encode_ok(&jobs.clear().to_string()),
                            F::UndoClear => {
                                if jobs.undo_clear() {
                                    control::encode_ok("ok")
                                } else {
                                    control::encode_err("nothing to undo")
                                }
                            }
                            F::QuitKeep => {
                                jobs.set_ask_quit(false);
                                control::encode_ok("ok")
                            }
                            // HushMic quits once they are saved (the loop
                            // looks each turn: Jobs::quit_ready)
                            F::QuitSave => match jobs.save_all_and_quit() {
                                Ok(n) => control::encode_ok(&n.to_string()),
                                Err(e) => control::encode_err(&e.to_string()),
                            },
                            F::QuitCancel => {
                                if jobs.cancel_quit() {
                                    control::encode_ok("ok")
                                } else {
                                    control::encode_err("not quitting")
                                }
                            }
                            // Answered above.
                            F::QuitStop | F::QuitAsk => control::encode_ok("stopping"),
                            F::Open => control::encode_ok("ok"),
                            F::NextToOriginal => {
                                jobs.set_folder(hushmic::sandbox::is_flatpak(), None);
                                cfg.files_folder = None;
                                saver.save(&cfg);
                                control::encode_ok("ok")
                            }
                            F::Folder(dir) => {
                                let dir = control::files_folder_path(&req.line).unwrap_or(dir);
                                let dir = std::path::PathBuf::from(dir);
                                if !dir.is_absolute() {
                                    control::encode_err(&format!(
                                        "not an absolute path: {}",
                                        dir.display()
                                    ))
                                } else if dir.is_dir() {
                                    jobs.set_folder(
                                        hushmic::sandbox::is_flatpak(),
                                        Some(dir.clone()),
                                    );
                                    cfg.files_folder = Some(dir);
                                    saver.save(&cfg);
                                    control::encode_ok("ok")
                                } else {
                                    control::encode_err(&format!("not a folder: {}", dir.display()))
                                }
                            }
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    Ok(control::Request::Retry) => {
                        // "Try again": the watchdog's next look comes now,
                        // and its backoff starts over.
                        backoff = hushmic::watchdog::Backoff::new();
                        let _ = tx.send(Event::Tick);
                        // the reply says what is done
                        let down = !controller.is_running()
                            || last_node_present == Some(false)
                            || cfg.mic.is_some() && controller.active_mic() != cfg.mic.as_deref();
                        let reply = retry_reply(cfg.enabled, down);
                        let _ = req.reply.send(control::encode_ok(reply));
                        continue;
                    }
                    Ok(control::Request::MicCheck) => {
                        let msg = match start_fallback_mictest(
                            &cfg,
                            &mut testing,
                            &mut mictest_cancel,
                            &tx,
                        ) {
                            Ok(()) => control::encode_ok(&format!(
                                "recording {} seconds now: speak into your microphone. \
                                     HushMic then plays the raw take and the cleaned one.",
                                mictest::RECORD_SECS
                            )),
                            Err(e) => control::encode_err(&e),
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    Ok(control::Request::MicCheckState) => {
                        let state = if testing { "running" } else { &mictest_end };
                        let _ = req.reply.send(control::encode_ok(state));
                        continue;
                    }
                    Ok(control::Request::SetupShortcuts) => {
                        let msg = if shortcuts_up == Some(true) {
                            let _ = shortcuts_cmd.send(if cfg.shortcuts_setup {
                                shortcuts::Cmd::Configure
                            } else {
                                shortcuts::Cmd::Bind
                            });
                            control::encode_ok("opening the shortcut settings")
                        } else {
                            control::encode_err("shortcuts are not available on this desktop")
                        };
                        let _ = req.reply.send(msg);
                        continue;
                    }
                    Ok(control::Request::HideNotice { app, name }) => {
                        if !cfg.hidden_notices.iter().any(|h| h.app == app) {
                            cfg.hidden_notices.push(hushmic::config::HiddenNotice {
                                app: app.clone(),
                                name,
                            });
                        }
                        watched.direct.retain(|d| d.app.id != app);
                        let _ = req
                            .reply
                            .send(control::encode_ok(&format!("hidden for {app}")));
                        save_noting(&saver, &cfg, &tx);
                        continue;
                    }
                    Ok(control::Request::ShowNotice { app }) => {
                        cfg.hidden_notices.retain(|h| h.app != app);
                        let _ = req
                            .reply
                            .send(control::encode_ok(&format!("shown for {app}")));
                        save_noting(&saver, &cfg, &tx);
                        continue;
                    }
                    Ok(control::Request::ResetProfile { device }) => {
                        if !cfg.mic_prefs.contains_key(&device) {
                            let _ = req.reply.send(control::encode_err(&format!(
                                "{device} has no settings of its own"
                            )));
                            continue;
                        }
                        let mut new = cfg.clone();
                        new.mic_prefs.remove(&device);
                        // Only the chain running on those settings restarts.
                        let in_use = controller
                            .active_profile()
                            .and_then(|p| p.device.as_deref())
                            == Some(device.as_str());
                        let res = if in_use {
                            target_cache = None;
                            apply_config(
                                &mut cfg,
                                new,
                                &mut controller,
                                &apply,
                                testing,
                                &mictest_cancel,
                                &mut gate,
                                false,
                            )
                        } else {
                            cfg = new;
                            Ok(())
                        };
                        // Only a restart that failed is an engine failure;
                        // a config write error is not.
                        if let Err(e) = &res {
                            watched
                                .lifecycle
                                .failed(e, hushmic::diagnostics::engine_generation());
                        }
                        let _ = req.reply.send(match res {
                            Ok(()) => control::encode_ok(&format!("{device} uses the defaults")),
                            Err(e) => control::encode_err(&e),
                        });
                        save_noting(&saver, &cfg, &tx);
                        refresh_tray(
                            &handle,
                            &cfg,
                            &mut controller,
                            &known_mics,
                            last_node_present,
                            &mut tray_engine,
                            &mut target_cache,
                        );
                        start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
                        continue;
                    }
                    // A late word from a window that is gone (replaced or
                    // closed) is not about the one open now; the CLI's
                    // `ui …` (tests, scripts) still counts.
                    Ok(control::Request::Ui(_)) if window_retired(req.peer) => {
                        let _ = req.reply.send(control::encode_ok("ok"));
                        continue;
                    }
                    Ok(control::Request::Ui(u)) => {
                        use control::UiRequest as U;
                        match u {
                            U::Page(p) => {
                                watched.ui.page = p;
                                if let Some(c) = companion.as_mut() {
                                    c.page = p;
                                }
                                sync_window_shows(&companion);
                            }
                            U::FirstRun(step) => {
                                watched.ui.first_run_step = step;
                                if step.is_some() {
                                    watched.ui.page = hushmic::appstate::Page::FirstRun;
                                    if let Some(c) = companion.as_mut() {
                                        c.page = hushmic::appstate::Page::FirstRun;
                                    }
                                    sync_window_shows(&companion);
                                }
                            }
                            U::BeamUsed => watched.ui.beam_used = true,
                            U::File(f) => watched.ui.file = f,
                            U::Size(w, h) => watched.ui.size = Some([w, h]),
                            // Only from the window itself: a word from a
                            // replaced one is late, not about this one.
                            U::Focused(on) => {
                                if let Some(c) =
                                    companion.as_mut().filter(|c| c.child.id() == req.peer)
                                {
                                    c.focused = on;
                                    c.settling = false;
                                }
                                sync_window_shows(&companion);
                            }
                            // the question the tray's Quit or Off asked
                            U::Answer(yes) => match watched.ask.take() {
                                Some(hushmic::appstate::Ask::Quit) if yes => {
                                    let _ = tx.send(Event::Cmd(TrayCmd::Quit));
                                }
                                Some(hushmic::appstate::Ask::TurnOff) if yes => {
                                    let _ = tx.send(Event::Cmd(TrayCmd::SetMode(None)));
                                }
                                _ => {}
                            },
                            U::Shows(p) => {
                                if let Some(c) =
                                    companion.as_mut().filter(|c| c.child.id() == req.peer)
                                {
                                    c.live = p
                                        .map_or(notify::LiveShows::Nothing, notify::LiveShows::One);
                                }
                                sync_window_shows(&companion);
                            }
                        }
                        watched.save_ui();
                        let _ = req.reply.send(control::encode_ok("ok"));
                        continue;
                    }
                    Ok(control::Request::SetMode(sel)) => {
                        control_reply = Some(req.reply);
                        shortcut_holds.user_mode_change();
                        Event::Cmd(TrayCmd::SetMode(sel))
                    }
                    Ok(control::Request::Toggle(target)) => {
                        let cur = cfg.enabled.then(|| controller.mode());
                        let sel = control::toggle_next(cur, prev_alive, target);
                        control_reply = Some(req.reply);
                        shortcut_holds.user_mode_change();
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
                    PE::Backend(b) => {
                        watched.shortcuts.backend = match b {
                            shortcuts::Backend::Portal => ShortcutsBackend::Portal,
                            shortcuts::Backend::KGlobalAccel => ShortcutsBackend::KdeService,
                            shortcuts::Backend::Unsupported => ShortcutsBackend::Unsupported,
                        };
                        continue;
                    }
                    PE::Available => {
                        watched.shortcuts.available = true;
                        shortcuts_up = Some(true);
                        shortcuts_backoff.record(true);
                        continue;
                    }
                    PE::Keys(keys) => {
                        for (action, k) in keys {
                            if let Some(e) = watched
                                .shortcuts
                                .keys
                                .iter_mut()
                                .find(|e| e.action == action)
                            {
                                e.keys = k;
                            }
                        }
                        continue;
                    }
                    PE::Unavailable => {
                        watched.shortcuts.available = false;
                        // A new session reports them again.
                        watched.shortcuts.keys = ShortcutsState::unreported();
                        shortcuts_up = Some(false);
                        shortcuts_backoff.record(false);
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
                        // Unsaved, the next start neither re-binds silently
                        // nor shows "Change shortcuts…": the writer leaves
                        // the one user hitting this a trace.
                        saver.save(&cfg);
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
                        Some(sel) => {
                            mute_hold = shortcuts::holds_mute(a, true);
                            Event::Cmd(TrayCmd::SetMode(sel))
                        }
                        None => continue,
                    },
                    PE::Deactivated(a) => match transition(a, false, &mut shortcut_holds) {
                        Some(sel) => Event::Cmd(TrayCmd::SetMode(sel)),
                        None => continue,
                    },
                }
            }
            // A mode change from the tray (the CLI's are marked above)
            // makes whatever push-to-mute had muted the user's own choice.
            other => {
                if matches!(other, Event::Cmd(TrayCmd::SetMode(_))) {
                    shortcut_holds.user_mode_change();
                }
                other
            }
        };
        match ev {
            // Quit or Off in the tray while the window asks already: the
            // question comes to the front, nothing more. A window that
            // can't open: go ahead.
            Event::Cmd(cmd @ (TrayCmd::AskQuit | TrayCmd::AskOff))
                if tray_question_up(jobs.asking_quit(), watched.ask) =>
            {
                let page = if jobs.asking_quit() {
                    hushmic::appstate::Page::Files
                } else {
                    companion_page(&mut companion, &watched.ui, jobs.at_stake() > 0)
                };
                let look = window_look(&mut desktop, &tx, &mut watched, &cfg, &[]);
                if !open_companion(&mut companion, page, watched.ui.size, look, &[], &tx) {
                    watched.ask = None;
                    let _ = tx.send(Event::Cmd(match cmd {
                        TrayCmd::AskQuit => TrayCmd::Quit,
                        _ => TrayCmd::SetMode(None),
                    }));
                }
            }
            // Quit in the tray while files clean: the window asks first
            // (the quit sheet). With nothing to stop, or a window that
            // can't open: Quit.
            Event::Cmd(TrayCmd::AskQuit)
                if tray_quit(jobs.at_stake(), jobs.asking_quit()) == TrayQuit::Ask =>
            {
                jobs.set_ask_quit(true);
                // the question is in the Files page's header
                let page = hushmic::appstate::Page::Files;
                let look = window_look(&mut desktop, &tx, &mut watched, &cfg, &[]);
                if !open_companion(&mut companion, page, watched.ui.size, look, &[], &tx) {
                    let _ = controller.disable();
                    break;
                }
            }
            // Quit or Off in the tray while an app uses HushMic: the window
            // asks first, as its own Quit does (UX panel A3). A window that
            // can't open: go ahead.
            Event::Cmd(cmd @ (TrayCmd::AskQuit | TrayCmd::AskOff))
                if tray_asks(&cmd, !watched.using.is_empty(), watched.ask, cfg.enabled) =>
            {
                watched.ask = Some(match cmd {
                    TrayCmd::AskQuit => hushmic::appstate::Ask::Quit,
                    _ => hushmic::appstate::Ask::TurnOff,
                });
                let page = companion_page(&mut companion, &watched.ui, jobs.at_stake() > 0);
                let look = window_look(&mut desktop, &tx, &mut watched, &cfg, &[]);
                if !open_companion(&mut companion, page, watched.ui.size, look, &[], &tx) {
                    watched.ask = None;
                    let _ = tx.send(Event::Cmd(match cmd {
                        TrayCmd::AskQuit => TrayCmd::Quit,
                        _ => TrayCmd::SetMode(None),
                    }));
                }
            }
            Event::Cmd(TrayCmd::AskOff) => {
                watched.ask = None;
                let _ = tx.send(Event::Cmd(TrayCmd::SetMode(None)));
            }
            Event::Cmd(TrayCmd::AskQuit) => {
                let _ = notify_direct_after(&hub, true);
                let _ = controller.disable();
                break;
            }
            // "Open HushMic", or a click on the icon: the window, on top.
            Event::Cmd(TrayCmd::Open) => {
                let page = companion_page(&mut companion, &watched.ui, jobs.at_stake() > 0);
                let look = window_look(&mut desktop, &tx, &mut watched, &cfg, &[]);
                open_companion(&mut companion, page, watched.ui.size, look, &[], &tx);
            }
            Event::Cmd(TrayCmd::OpenFiles) => {
                let page = match companion_page(&mut companion, &watched.ui, jobs.at_stake() > 0) {
                    // first run under way keeps its place
                    hushmic::appstate::Page::FirstRun => hushmic::appstate::Page::FirstRun,
                    _ => hushmic::appstate::Page::Files,
                };
                let look = window_look(&mut desktop, &tx, &mut watched, &cfg, &[]);
                open_companion(&mut companion, page, watched.ui.size, look, &[], &tx);
            }
            Event::Cmd(cmd) => {
                // Every command may move or stop the chain: the cached
                // no-chain device is resolved again when needed.
                target_cache = None;
                // Any command that will re-render/restart the chain (or tear
                // it down) invalidates a running mic test's cleaned leg —
                // cancel it rather than let it record a dead node.
                // A live mode switch (SetMode(Some) with a running chain)
                // deliberately does NOT count: it flips a control on the
                // running node without restarting anything. Its rare
                // set-param-failed fallback DOES restart — that path re-runs
                // this invalidation inline before applying.
                // A strength or model change goes through apply_mic, which
                // invalidates only when the chain really restarts (a
                // strength change is a live write).
                let mutates_chain = matches!(cmd, TrayCmd::SetMode(None))
                    || (!cfg.enabled && matches!(cmd, TrayCmd::SetMode(Some(_))))
                    || (cfg.enabled && matches!(cmd, TrayCmd::SelectMic(_)));
                if testing && mutates_chain {
                    if let Some(c) = &mictest_cancel {
                        c.store(true, Ordering::Relaxed);
                    }
                }
                let mut applied: Result<(), String> = Ok(());
                // Off while apps used HushMic: what they hear now, for the
                // command line too (UX panel A1)
                let mut off_note: Option<String> = None;
                match cmd {
                    TrayCmd::SetMode(sel) => {
                        let old_sel = cfg.enabled.then(|| controller.mode());
                        if sel.is_some() && !cfg.enabled {
                            // Turned on deliberately: "Set as default
                            // microphone" applies again, and a recovery
                            // fallback from before ends (the saved mic may
                            // have been replugged meanwhile; if not, recovery
                            // falls back again).
                            watched.start_why = Start::TurnOn;
                            watched.released_to = None;
                            watched.turned_on();
                            notify::close(notify::Slot::Power);
                            controller.reclaim_default();
                            controller.end_mic_fallback();
                        }
                        match sel {
                            None => {
                                if cfg.enabled {
                                    off_note = notify_direct_after(&hub, false);
                                    watched.turned_off();
                                }
                                cfg.enabled = false;
                                applied = apply(&mut controller, &cfg);
                            }
                            Some(m) => {
                                let live = cfg.enabled && controller.is_running();
                                controller.set_mode_state(m);
                                cfg.enabled = true;
                                if live && controller.write_mode_live() {
                                    // switched on the running chain (its
                                    // control file at once, its port off the
                                    // loop) - no restart, no audible gap
                                } else {
                                    if live {
                                        eprintln!(
                                            "[hushmic] the chain stopped; \
                                         restarting it with mode {m:?}"
                                        );
                                        // the restart invalidates what the
                                        // mutates_chain gate above skipped for
                                        // the live path
                                        if testing {
                                            if let Some(c) = &mictest_cancel {
                                                c.store(true, Ordering::Relaxed);
                                            }
                                        }
                                    }
                                    applied = apply(&mut controller, &cfg);
                                }
                            }
                        }
                        (prev_alive, unmute_stored) = control::after_transition(
                            prev_alive,
                            unmute_stored,
                            old_sel,
                            sel,
                            mute_hold,
                        );
                    }
                    TrayCmd::SelectMic(m) => {
                        // The restart applies the pick's profile; the
                        // refresh below shows it in the radios. A pick
                        // ends a recovery fallback (it restarts anyway).
                        controller.end_mic_fallback();
                        cfg.apply_mic_selection(m);
                        if cfg.enabled {
                            applied = apply(&mut controller, &cfg);
                        }
                    }
                    TrayCmd::SetAttn(v) => {
                        let _ = controller.settle_in_use(&cfg);
                        let device = settings_device(&cfg, &controller, &mut target_cache);
                        cfg.set_attn_for(device.as_deref(), v);
                        if cfg.enabled {
                            applied = apply_mic(&mut controller, &cfg, testing, &mictest_cancel);
                        }
                    }
                    TrayCmd::Quit => {
                        let _ = notify_direct_after(&hub, true);
                        let _ = controller.disable();
                        break;
                    }
                    // Answered above, before the settings bookkeeping.
                    TrayCmd::Open | TrayCmd::OpenFiles | TrayCmd::AskQuit | TrayCmd::AskOff => {}
                }
                if let Err(e) = &applied {
                    eprintln!("hushmic: enable failed: {e}");
                    watched
                        .lifecycle
                        .failed(e, hushmic::diagnostics::engine_generation());
                    if gate.on_enable_error(e, true) {
                        notify::send_problem(
                            notify::Problem::Failure,
                            "dialog-error",
                            &fail_summary(),
                            e,
                        );
                    }
                } else if !cfg.enabled {
                    // user turned it off: stale failure state must not
                    // produce a "running again" notice later
                    gate.reset();
                    notify::forget_failure_bubble();
                }
                // A CLI-originated SetMode gets its answer now that the
                // outcome is known: the resulting mode word, or the error.
                if let Some(reply) = control_reply.take() {
                    let msg = match &applied {
                        Ok(()) => control::encode_ok(&control::with_note(
                            control::mode_word(cfg.enabled.then(|| controller.mode())),
                            off_note.as_deref(),
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
                // a mode change isn't stored: nothing is written then
                saver.save(&cfg);
                // reflect updated state + refreshed mic list + status in the tray
                refresh_tray(
                    &handle,
                    &cfg,
                    &mut controller,
                    &known_mics,
                    last_node_present,
                    &mut tray_engine,
                    &mut target_cache,
                );
                start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
            }
            Event::ShowWindow(env) => {
                // A relaunch: the window comes up on top; an open one is
                // re-created on the page it showed.
                let page = companion_page(&mut companion, &watched.ui, jobs.at_stake() > 0);
                let look = window_look(&mut desktop, &tx, &mut watched, &cfg, &env);
                open_companion(&mut companion, page, watched.ui.size, look, &env, &tx);
            }
            Event::MicTestDone(res) => {
                testing = false;
                mictest_cancel = None;
                mictest_end = match &res {
                    Ok(()) => "done".into(),
                    Err(e) if e == mictest::CANCELLED_MSG => "cancelled".into(),
                    Err(e) => format!("failed {e}"),
                };
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
            }
            Event::Tick => {
                // A microphone silent to an app for 10 s (a hardware mute
                // switch, a dead cable): the tray says so, and one
                // notification an episode, held while the window shows it.
                let silent_now = mic_silent_now(
                    cfg.enabled && controller.is_running(),
                    !watched.using.is_empty(),
                    || silence.silent(controller.meter_path(), Instant::now()),
                );
                if silent_now && !mic_silent {
                    let state = hub.last();
                    let active = state.as_ref().and_then(|s| s.mic.active.clone());
                    let mic = state
                        .as_ref()
                        .zip(active.as_deref())
                        .and_then(|(s, a)| s.description_of(a).map(str::to_string))
                        .unwrap_or_else(|| tr!("notify-your-mic"));
                    eprintln!("[hushmic] no sound from the microphone for 10 s");
                    // the cheap way out: the one other microphone, else
                    // the window (UX panel round 2, A5)
                    let action = match silent_mic_way_out(&known_mics, active.as_deref()) {
                        Some(other) => (
                            format!("use-mic:{}", other.name),
                            tr!("notify-use-mic", mic = other.description.clone()),
                        ),
                        None => ("open".to_string(), tr!("notify-open-hushmic")),
                    };
                    notify::send_problem_with_actions(
                        notify::Problem::MicSilent,
                        "audio-input-microphone",
                        &tr!("live-title-silent", mic = mic),
                        &tr!("live-sub-silent"),
                        &[action],
                    );
                } else if mic_silent && !silent_now {
                    // sound is back: the notice goes
                    notify::close(notify::Slot::Silence);
                }
                mic_silent = silent_now;
                if let Some(generation) = ports_owed.take() {
                    if controller.chain_generation() == Some(generation)
                        && cfg.enabled
                        && controller.is_running()
                    {
                        let starting = controller
                            .secs_since_spawn()
                            .is_some_and(|s| s < STARTUP_GRACE_SECS);
                        let running = node_state_gen == generation
                            && last_node_state.as_deref() == Some("running");
                        match controller.port_missed(running, starting) {
                            portwriter::Missed::Nothing => {}
                            portwriter::Missed::Wait => ports_owed = Some(generation),
                            // A failure of this write comes back as
                            // PortWriteFailed and decides the restart.
                            portwriter::Missed::Retry | portwriter::Missed::Restart => {
                                controller.rewrite_ports()
                            }
                        }
                    }
                }
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
                                    &known_mics,
                                    last_node_present,
                                    &mut tray_engine,
                                    &mut target_cache,
                                );
                                // The loop head pushes the status line.
                                tray_extras = None;
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
                // Reap the companion window; what Mic check left in the
                // runtime dir goes with it. A window that could not open at
                // all (no display, no GL: it never reached its first frame)
                // says so; one the user closed (it exits 0; the daemon's own
                // replacements and quits never get here) is told about once;
                // one that ran and then died (its display went away at a
                // logout, a crash) is only logged.
                let mut companion_ended = None;
                if let Some(Companion {
                    child,
                    spawned,
                    session,
                    ..
                }) = companion.as_mut()
                {
                    if let Ok(Some(status)) = child.try_wait() {
                        eprintln!(
                            "[hushmic] window exited ({status}) after {:.1}s",
                            spawned.elapsed().as_secs_f32()
                        );
                        retire_window(child.id());
                        let session = session.get().cloned().flatten();
                        companion_ended = Some((window_end(status), session));
                    }
                }
                if let Some((end, session)) = companion_ended {
                    companion = None;
                    // a question the window did not answer is withdrawn
                    watched.ask = None;
                    sync_window_shows(&companion);
                    hushmic::miccheck::remove_files();
                    match quit_question_after(&end, jobs.asking_quit()) {
                        QuitQuestion::Quit => {
                            let _ = tx.send(Event::Cmd(TrayCmd::Quit));
                        }
                        QuitQuestion::Withdraw => jobs.set_ask_quit(false),
                        QuitQuestion::Keep => {}
                    }
                    match end {
                        WindowEnd::CouldNotOpen => notify_window_failed(),
                        WindowEnd::Closed => arm_first_close(&mut first_close, &cfg, &tx, session),
                        WindowEnd::Died => {}
                    }
                }
                // One pw-dump snapshot per tick serves the liveness check, the
                // tray status, a hotplug refresh of the mic list (menu clicks
                // are far too rare to be the only refresh trigger), AND the
                // capture-stream feeder check: read off the loop, handled
                // below when it lands.
                if let Some(t) = graph_probe {
                    if t.elapsed() >= GRAPH_SLOW && !graph_slow_logged {
                        graph_slow_logged = true;
                        eprintln!(
                            "[hushmic] PipeWire has not answered a graph read for {} s \
                             (session manager busy?); mode and strength still apply",
                            t.elapsed().as_secs()
                        );
                    }
                }
                tick_owed = true;
                start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
            }
            Event::Look => {
                if graph_probe.is_some() {
                    graph_again = true;
                } else {
                    start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
                }
            }
            Event::Graph {
                dump,
                generation,
                #[cfg(feature = "speakers")]
                speakers_generation,
                landed_before,
            } => {
                graph_probe = None;
                if std::mem::take(&mut graph_again) {
                    start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
                }
                if dump.is_some() && std::mem::take(&mut graph_slow_logged) {
                    eprintln!("[hushmic] PipeWire answers again");
                }
                // Every snapshot is a look for the speakers cleaner too:
                // its output, its sink, a chain that died.
                #[cfg(feature = "speakers")]
                speakers.look_asked(&cfg, dump.as_deref(), speakers_generation);
                // The session manager answers again after a read that
                // failed or timed out: a takeover that missed for want of an
                // answer is due at once (its count of misses stays).
                if dump.is_none() {
                    graph_unanswered = true;
                } else if std::mem::take(&mut graph_unanswered) {
                    controller.session_manager_answered();
                }
                // A chain started or stopped while the probe was out: the
                // snapshot may predate it. Read again.
                if generation != hushmic::diagnostics::engine_generation() {
                    start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
                    continue;
                }
                // This snapshot is the tick's look (see tick_owed).
                let ticked = std::mem::take(&mut tick_owed);
                // Whether the takeover had landed before this snapshot: one
                // that completes later is not in it.
                let taken_before_dump = landed_before;
                let nodes = dump.as_deref().map(pipewire::parse_pwdump_nodes);
                let graph = dump.as_deref().map(pipewire::chain_graph);
                let node_present = graph.as_ref().map(pipewire::ChainGraph::up);
                last_node_present = node_present;
                last_node_state = dump
                    .as_deref()
                    .and_then(|d| pipewire::node_state(d, pipewire::CHAIN_INPUT));
                // Only our link belongs on the chain's output and the host's
                // feed; one that went missing is made again (never a
                // respawn: both ends are up).
                if let Some(g) = &graph {
                    controller.unlink(g.foreign.clone());
                    // an app's playback linked into the capture or the
                    // feed (its sound would go into the call): its link is
                    // cut, and it goes to the default as well
                    let steps = watched.guard.observe(&g.intruders, &g.streams);
                    let moving: Vec<u32> = steps
                        .iter()
                        .filter_map(|s| match s {
                            pipewire::GuardStep::Move { id, .. } => Some(*id),
                            _ => None,
                        })
                        .collect();
                    controller.cut(watched.guard.to_cut(&g.cuts, &moving));
                    for step in steps {
                        match step {
                            pipewire::GuardStep::Move { id, name } => {
                                let moved = pipewire::move_to_default(id);
                                eprintln!(
                                    "[hushmic] {} was linked into HushMic's microphone input: \
                                     link cut, {}",
                                    if name.is_empty() { "a stream" } else { &name },
                                    if moved {
                                        "moved to the default"
                                    } else {
                                        "could not be moved (pw-metadata error)"
                                    }
                                );
                            }
                            pipewire::GuardStep::KeptOff { name } => eprintln!(
                                "[hushmic] {} keeps being linked into HushMic's microphone \
                                 input: its link is cut each time",
                                if name.is_empty() { "a stream" } else { &name }
                            ),
                        }
                    }
                    if cfg.enabled && controller.is_running() && g.chain() && g.host() && !g.linked
                    {
                        controller.relink();
                    }
                }
                node_state_gen = generation;
                // The window's view of the graph: is the chain up, who
                // records HushMic, who records the cleaned mic directly.
                watched
                    .lifecycle
                    .observe(hushmic::diagnostics::engine_generation(), node_present);
                match dump.as_deref() {
                    Some(d) => {
                        watched.observe_graph(d, &mut controller);
                        if cfg.enabled {
                            watched.return_streams(d, &tx);
                        }
                        let running = controller.is_running();
                        let cleaned = controller
                            .active_mic()
                            .map(str::to_string)
                            .or_else(|| watched.system_default.clone())
                            .or_else(|| watched.feeder.clone());
                        let readers = cleaned
                            .as_deref()
                            .map(|m| pipewire::parse_readers(d, m))
                            .unwrap_or_default();
                        let hidden = |id: &str| cfg.hidden_notices.iter().any(|h| h.app == id);
                        watched.direct = watched.direct_watch.observe(
                            &readers,
                            cfg.enabled && running && cleaned.is_some(),
                            &hidden,
                            &apps::is_ours,
                            &watched.names,
                        );
                    }
                    None => watched.pw_failed(&tx),
                }
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
                // grace for a theft: it happens AT stream creation — the
                // early tick a spawn schedules heals it before the window's
                // start wears out its welcome. A stream fed by nothing is
                // left alone while the chain starts (the session manager's
                // initial link setup) and for REPIN_UNLINKED after that: a
                // move whose new link failed leaves it unlinked for good on
                // WirePlumber 0.4, and only a re-pin makes it look again (a
                // stuck one, only a chain restart: pipewire::Repin).
                if repin_supported && cfg.enabled && controller.is_running() {
                    if let Some(dump) = dump.as_deref() {
                        let feeders = pipewire::parse_feeders(dump, "hushmic_input");
                        // Fed by nothing while the chain starts is the
                        // session manager linking it; later, a link that
                        // failed (pipewire::Look::Unlinked).
                        let starting = controller
                            .secs_since_spawn()
                            .is_some_and(|s| s < STARTUP_GRACE_SECS);
                        if !feeders.is_empty() || !starting {
                            let want = pipewire::repin_want(
                                controller.active_mic(),
                                pipewire::parse_default_source(dump),
                                controller.prior_default(),
                            );
                            if let Some(want) = want {
                                // Both ids come from the same snapshot, so
                                // they are mutually consistent; a respawn
                                // racing this look lands the write on a dead
                                // id and the next look corrects it.
                                let ids = (
                                    pipewire::parse_node_id(dump, "hushmic_input"),
                                    pipewire::parse_node_serial(dump, &want),
                                );
                                let look = pipewire::capture_look(
                                    &feeders,
                                    &want,
                                    ids.0.is_some(),
                                    ids.1.is_some(),
                                );
                                let stuck_before = repin.gave_up();
                                let now = Instant::now();
                                let step = look.map(|l| repin.observe(l, now));
                                // Restarts did not get the mic linked: no
                                // more of them (the slow re-pin goes on);
                                // say so, here and in Live's Status.
                                if repin.gave_up() && !stuck_before {
                                    eprintln!(
                                        "[hushmic] the capture stream's link to {want} is \
                                         still stuck after {} chain restarts — no more \
                                         restarts; re-pinning once a minute",
                                        pipewire::REPIN_RESTARTS
                                    );
                                    notify::send_problem(
                                        notify::Problem::MicStuck,
                                        "audio-input-microphone",
                                        &tr!("notify-mic-stuck-summary"),
                                        &tr!("notify-mic-stuck-body"),
                                    );
                                }
                                // A correction to check, a wait that runs out:
                                // looked at then, not at the 5 s tick (only
                                // after a look: see Repin::owed_look).
                                if let Some(due) = repin.owed_look(look.is_some(), now) {
                                    if healer_look != Some(due) {
                                        healer_look = Some(due);
                                        let tx = tx.clone();
                                        std::thread::spawn(move || {
                                            std::thread::sleep(
                                                due.saturating_duration_since(Instant::now()),
                                            );
                                            let _ = tx.send(Event::HealerLook(due));
                                        });
                                    }
                                }
                                if let (
                                    Some(pipewire::RepinStep::Repin { notify }),
                                    (Some(id), Some(_)),
                                ) = (step, ids)
                                {
                                    // Three corrections in and stolen again:
                                    // something re-asserts its route after
                                    // every one (EasyEffects' process-all-
                                    // inputs). Fighting it every time would
                                    // flap the mic — say so once, retry
                                    // gently.
                                    if notify {
                                        eprintln!(
                                            "[hushmic] another app keeps re-routing \
                                             HushMic's microphone (EasyEffects?) — \
                                             backing off to a slow retry"
                                        );
                                        notify::send_problem(
                                            notify::Problem::Rerouted,
                                            "audio-input-microphone",
                                            &tr!("notify-reroute-summary"),
                                            &tr!("notify-reroute-body"),
                                        );
                                    }
                                    if feeders.is_empty() {
                                        eprintln!(
                                            "[hushmic] the capture stream lost its \
                                             link — re-pinning to {want}"
                                        );
                                    } else {
                                        eprintln!(
                                            "[hushmic] the capture stream was re-routed \
                                             to {feeders:?} (another audio tool?) — \
                                             re-pinning to {want}"
                                        );
                                    }
                                    if !pipewire::repin_capture(id, &want) {
                                        eprintln!(
                                            "[hushmic] re-pin failed (pw-metadata \
                                             error)"
                                        );
                                    }
                                }
                                // A re-pin did not relink it: the link to
                                // the mic is stuck (pipewire::Repin). Only
                                // the chain restarts; apps stay on
                                // hushmic_source.
                                if step == Some(pipewire::RepinStep::Restart) {
                                    eprintln!(
                                        "[hushmic] the capture stream's link to {want} is \
                                         stuck — restarting the chain"
                                    );
                                    // a mic test would record a chain mid-restart
                                    if testing {
                                        if let Some(c) = &mictest_cancel {
                                            c.store(true, Ordering::Relaxed);
                                        }
                                    }
                                    watched.start_why = Start::Recovery;
                                    match controller.enable(&cfg) {
                                        Ok(()) => schedule_early_tick(&tx),
                                        Err(e) => {
                                            eprintln!("[hushmic] the chain restart failed: {e}")
                                        }
                                    }
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
                // Judged per process: the source host (hushmic_source and
                // its feed) and the chain (its capture and output). A
                // missing link is not "down" (relinked above), and neither
                // are ports the session manager has not set up yet.
                let in_grace = controller
                    .secs_since_spawn()
                    .is_some_and(|s| s < STARTUP_GRACE_SECS);
                let host_grace = controller
                    .secs_since_host_spawn()
                    .is_some_and(|s| s < STARTUP_GRACE_SECS);
                let host_down = !controller.host_running()
                    || (graph.as_ref().is_some_and(|g| !g.host()) && !host_grace);
                let chain_down = !controller.is_running()
                    || (graph.as_ref().is_some_and(|g| !g.chain()) && !in_grace);
                let down = cfg.enabled && (host_down || chain_down);
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
                        watched.start_why = Start::Watchdog;
                        // The host gone (or its nodes, at a PipeWire
                        // restart): everything starts over. Else only the
                        // chain, and apps stay on hushmic_source.
                        let started = if host_down {
                            controller.restart_all(&cfg)
                        } else {
                            controller.enable(&cfg)
                        };
                        // A link still being made (the session manager
                        // sets the ports up within LINK_LIMIT) is no
                        // verdict yet: it counts as a respawn, but the
                        // backoff and "up" wait for the tick's healthy
                        // branch (or the chain found down after grace).
                        let pending = matches!(started, Ok(()))
                            && controller.is_running()
                            && controller.wait_link(Duration::from_secs(2)).is_none();
                        let ok = match started {
                            Ok(()) if pending => true,
                            Ok(()) => {
                                let up = controller.is_running()
                                    && controller.wait_linked(Duration::ZERO);
                                if !up {
                                    watched.lifecycle.failed(
                                        "the HushMic microphone did not appear in PipeWire",
                                        hushmic::diagnostics::engine_generation(),
                                    );
                                }
                                // enable() succeeded yet the node never came:
                                // nothing ever reaches stderr on this path, so
                                // a stuck loop must be surfaced explicitly.
                                if !up && gate.on_silent_failure() {
                                    notify::send_problem(
                                        notify::Problem::Failure,
                                        "dialog-error",
                                        &tr!("notify-chain-stuck-summary"),
                                        &tr!("notify-chain-stuck-body"),
                                    );
                                }
                                up
                            }
                            Err(e) => {
                                eprintln!("hushmic: enable failed: {e}");
                                watched.lifecycle.failed(
                                    &e.to_string(),
                                    hushmic::diagnostics::engine_generation(),
                                );
                                if gate.on_enable_error(&e.to_string(), false) {
                                    notify::send_problem(
                                        notify::Problem::Failure,
                                        "dialog-error",
                                        &fail_summary(),
                                        &e.to_string(),
                                    );
                                }
                                false
                            }
                        };
                        if !pending {
                            backoff.record(ok);
                        }
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
                                notify::send_problem(
                                    notify::Problem::Flapping,
                                    "dialog-error",
                                    &tr!("notify-chain-flapping-summary"),
                                    &tr!("notify-chain-flapping-body"),
                                );
                            }
                            // up again: the window's "running again" now
                            // (not mid-flap; the notification waits); a
                            // link still being made gets it from the
                            // healthy branch
                            if !pending {
                                gate.on_up();
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
                            notify::send_problem(
                                notify::Problem::Recovered,
                                "audio-input-microphone",
                                &tr!("notify-running-again-summary"),
                                &tr!("notify-running-again-body"),
                            );
                        }
                        // the window's "running again" (engine.recovered)
                        // comes at once, the notification above once stable
                        gate.on_up();
                    } else if !cfg.enabled {
                        gate.reset();
                        notify::forget_failure_bubble();
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
                    // every read counts, at its time: an unplug is told
                    // within seconds (UX panel round 2, A5)
                    let decision = recovery.observe(
                        cfg.mic.is_some(),
                        controller.active_mic() == cfg.mic.as_deref(),
                        preferred_present,
                        in_grace,
                        Instant::now(),
                    );
                    // a switch pending: the read that decides it, on time
                    if let Some(due) = recovery.decides_at() {
                        if switch_look != Some(due) {
                            switch_look = Some(due);
                            let tx = tx.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(
                                    due.saturating_duration_since(Instant::now())
                                        + Duration::from_millis(50),
                                );
                                let _ = tx.send(Event::Look);
                            });
                        }
                    }
                    if let Some(switch) = decision {
                        // both microphones by name (UX panel B7): the
                        // chosen one, and the default that stands in
                        let label = |dev: Option<&str>| {
                            mic_label(&cfg, &controller, &known_mics, nodes.as_deref(), dev)
                        };
                        let chosen = label(cfg.mic.as_deref());
                        let (log_what, body) = match switch {
                            watchdog::Switch::Fallback => (
                                "preferred microphone disconnected",
                                tr!(
                                    "notify-mic-fallback-body",
                                    mic = chosen,
                                    standin = label(watched.system_default.as_deref())
                                ),
                            ),
                            watchdog::Switch::Return => (
                                "preferred microphone reconnected",
                                tr!(
                                    "notify-mic-return-body",
                                    mic = chosen,
                                    standin = label(
                                        watched
                                            .feeder
                                            .as_deref()
                                            .filter(|f| Some(*f) != cfg.mic.as_deref())
                                            .or(watched.system_default.as_deref())
                                    )
                                ),
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
                        let started = Instant::now();
                        watched.start_why = Start::Recovery;
                        if switch == watchdog::Switch::Fallback {
                            controller
                                .stand_in(stand_in(cfg.mic.as_deref(), watched.feeder.as_deref()));
                        }
                        controller.set_mic_fallback(switch == watchdog::Switch::Fallback);
                        let result = controller.enable(&cfg);
                        if started.elapsed() > Duration::from_secs(2) {
                            eprintln!(
                                "[hushmic] the chain restart took {} ms",
                                started.elapsed().as_millis()
                            );
                        }
                        match result {
                            Ok(()) => {
                                schedule_early_tick(&tx);
                                notify::send_problem_transient(
                                    if switch == watchdog::Switch::Fallback {
                                        notify::Problem::MicFallback
                                    } else {
                                        notify::Problem::MicBack
                                    },
                                    "audio-input-microphone",
                                    "HushMic",
                                    &body,
                                );
                            }
                            Err(e) => {
                                eprintln!("hushmic: enable failed: {e}");
                                watched.lifecycle.failed(
                                    &e.to_string(),
                                    hushmic::diagnostics::engine_generation(),
                                );
                                if gate.on_enable_error(&e.to_string(), false) {
                                    notify::send_problem(
                                        notify::Problem::Failure,
                                        "dialog-error",
                                        &fail_summary(),
                                        &e.to_string(),
                                    );
                                }
                            }
                        }
                    }
                }

                // --- per-mic profiles: a chain that follows the default
                // source re-links on its own when the default moves, but
                // its model and strength were rendered for the device it
                // started on. watchdog::ProfileFollow decides (debounce,
                // cooldown, compared against what the running chain
                // resolved); this only gathers the facts and executes.
                // With "Set as default microphone" active the chain
                // follows the pre-takeover device. A user who switches the
                // default away from HushMic keeps that switch: the
                // takeover is released first (watchdog::DefaultRelease),
                // so neither the settings restart nor a quit takes the
                // default back — until the next deliberate start.
                // No deferral while an app records: the wrong settings
                // for a whole call are worse than one short gap.
                let settling = controller
                    .secs_since_spawn()
                    .is_some_and(|s| s < STARTUP_GRACE_SECS);
                let moved_to = dump
                    .as_deref()
                    .and_then(pipewire::parse_configured_default_source)
                    .filter(|d| hushmic::config::can_own_profile(d));
                if ticked
                    && default_release.observe(
                        taken_before_dump && controller.default_landed(),
                        moved_to.is_some(),
                        settling,
                    )
                {
                    if let Some(to) = &moved_to {
                        // A takeover whose write never landed comes back
                        // false: nothing was taken, so there is nothing to
                        // tell (the controller logs it).
                        if controller.release_default(to) {
                            watched.released_to = Some(to.clone());
                            // Named as the window names it (twins
                            // numbered).
                            let name = controller
                                .mic_name(&cfg, &known_mics, to)
                                .or_else(|| {
                                    nodes
                                        .as_deref()
                                        .and_then(|v| pipewire::description_of(v, to))
                                        .map(str::to_string)
                                })
                                .unwrap_or_else(|| to.clone());
                            notify::send_problem(
                                notify::Problem::DefaultReleased,
                                "audio-input-microphone",
                                &tr!("notify-default-released-summary"),
                                &tr!("notify-default-released-open", device = name),
                            );
                        }
                    }
                }
                let running = controller.active_profile().cloned();
                let following = cfg.enabled
                    && !down
                    && controller.is_running()
                    && controller.active_mic().is_none()
                    && running.is_some();
                // the default it follows, or the device it fell back to
                // when that one is gone (UX panel A2)
                let observed = dump.as_ref().and(controller.in_use()).map(str::to_string);
                let want = observed.as_deref().map(|o| cfg.profile_for(Some(o)));
                let same_settings = match (&want, &running) {
                    (Some(w), Some(r)) => w.model == r.model && w.attn_limit == r.attn_limit,
                    _ => false,
                };
                // the microphone in use moved under System default while an
                // app uses HushMic: say which (UX panel B7), also when the
                // default one left (round 2, A8), at the read that sees it
                let now_following = observed.clone().filter(|_| following);
                if let (Some(old), Some(new)) = (&followed_seen, &now_following) {
                    if old != new && !watched.using.is_empty() {
                        let label = |dev: &str| {
                            mic_label(&cfg, &controller, &known_mics, nodes.as_deref(), Some(dev))
                        };
                        let gone = nodes
                            .as_deref()
                            .is_some_and(|v| !v.iter().any(|s| &s.name == old));
                        let body = if gone {
                            tr!(
                                "notify-mic-fallback-body",
                                mic = label(old),
                                standin = label(new)
                            )
                        } else {
                            tr!(
                                "notify-default-switched-body",
                                new = label(new),
                                old = label(old)
                            )
                        };
                        notify::send_transient(
                            notify::Slot::Mic,
                            "audio-input-microphone",
                            "HushMic",
                            &body,
                        );
                    }
                }
                // a read that sees no device (relinking) keeps the last one
                if !following {
                    followed_seen = None;
                } else if now_following.is_some() {
                    followed_seen = now_following;
                }
                let action = if ticked {
                    profile_follow.observe(watchdog::ProfileFacts {
                        following,
                        in_grace: settling,
                        running: running.as_ref().and_then(|r| r.device.as_deref()),
                        observed: observed.as_deref().map(Some),
                        same_settings,
                    })
                } else {
                    watchdog::ProfileAction::Keep
                };
                if let (Some(observed), Some(want)) = (observed.as_deref(), want) {
                    // Named as the window names it, when there is
                    // something to say.
                    let name = if matches!(action, watchdog::ProfileAction::Keep) {
                        None
                    } else {
                        controller
                            .mic_name(&cfg, &known_mics, observed)
                            .or_else(|| {
                                nodes
                                    .as_deref()
                                    .and_then(|v| pipewire::description_of(v, observed))
                                    .map(str::to_string)
                            })
                    };
                    match action {
                        watchdog::ProfileAction::Keep => {}
                        watchdog::ProfileAction::Retarget => {
                            controller.retarget_profile(want, name.as_deref());
                        }
                        watchdog::ProfileAction::GiveUp => {
                            eprintln!(
                                "[hushmic] the default microphone is {} but restarts \
                                 keep landing on another device; keeping the running \
                                 settings",
                                name.as_deref().unwrap_or(observed)
                            );
                        }
                        watchdog::ProfileAction::Restart => {
                            eprintln!(
                                "[hushmic] default microphone is now {}; restarting \
                                 the chain with its settings",
                                name.as_deref().unwrap_or(observed)
                            );
                            // Same invalidation as a settings change.
                            invalidate_mic_consumers(testing, &mictest_cancel);
                            match controller.enable(&cfg) {
                                Ok(()) => schedule_early_tick(&tx),
                                Err(e) => {
                                    eprintln!("hushmic: enable failed: {e}");
                                    watched.lifecycle.failed(
                                        &e.to_string(),
                                        hushmic::diagnostics::engine_generation(),
                                    );
                                    if gate.on_enable_error(&e.to_string(), false) {
                                        notify::send_problem(
                                            notify::Problem::Failure,
                                            "dialog-error",
                                            &fail_summary(),
                                            &e.to_string(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // reflect liveness in the tray status (icon + title) every tick
                let status = compute_status(&cfg, &mut controller, node_present);
                // Whose settings the radios show: the running chain's, or
                // with none, the device a start would use (from this
                // tick's snapshot — no extra probe).
                let shown_device = match controller.active_profile() {
                    Some(p) => Some(p.device.clone()),
                    None => dump.as_deref().map(|d| {
                        let mic =
                            pipewire::resolve_effective_mic(cfg.mic.as_deref(), nodes.as_deref());
                        let followed = pipewire::followed_default(d, None);
                        let device =
                            hushmic::config::profile_owner(mic.as_deref(), followed.as_deref())
                                .map(str::to_string);
                        target_cache = Some((cfg.mic.clone(), device.clone()));
                        device
                    }),
                };
                let profile = shown_device.map(|d| cfg.profile_for(d.as_deref()));
                let _ = handle.update(move |t: &mut HushMicTray| {
                    t.status = status;
                    if let Some(p) = profile {
                        t.profile = p;
                    }
                });
            }
            // Nothing to do but publish (the loop head does).
            Event::Wake => {}
            Event::Desktop(look) => watched.desktop = look,
            Event::PortWriteFailed(generation) => {
                // Only the running chain's, and only where its plugin does
                // not take the control file: then the port was the only
                // way in, and a restart renders the value instead. A
                // plugin that has not started yet takes the file when it
                // does (its reader word is unset until then).
                if controller.chain_generation() == Some(generation)
                    && cfg.enabled
                    && controller.is_running()
                {
                    let starting = controller
                        .secs_since_spawn()
                        .is_some_and(|s| s < STARTUP_GRACE_SECS);
                    let running = node_state_gen == generation
                        && last_node_state.as_deref() == Some("running");
                    let missed = controller.port_missed(running, starting);
                    if matches!(missed, portwriter::Missed::Wait | portwriter::Missed::Retry) {
                        ports_owed = Some(generation);
                    } else if missed == portwriter::Missed::Restart {
                        eprintln!(
                            "[hushmic] a live mode or strength change did not reach the chain; \
                             restarting it with the current settings"
                        );
                        invalidate_mic_consumers(testing, &mictest_cancel);
                        if let Err(e) = controller.enable(&cfg) {
                            let e = e.to_string();
                            eprintln!("hushmic: enable failed: {e}");
                            watched
                                .lifecycle
                                .failed(&e, hushmic::diagnostics::engine_generation());
                            if gate.on_enable_error(&e, true) {
                                notify::send_problem(
                                    notify::Problem::Failure,
                                    "dialog-error",
                                    &fail_summary(),
                                    &e,
                                );
                            }
                        }
                    }
                }
            }
            Event::TakeoverDone => controller.takeover_finished(),
            Event::Files(ev) => jobs.heard(ev),
            // the note's Save all; in a sandbox with no folder yet the
            // window asks where (Files, its Save all)
            Event::FilesSaveAll => {
                if jobs.save_all().is_err() {
                    let _ = tx.send(Event::Cmd(TrayCmd::OpenFiles));
                }
            }
            // A run began while no app was seen: look now, not at the next
            // tick, so the window shows the use (and its mark) at once.
            Event::RunBegan => {
                // At most one a second: an engine that keeps stopping and
                // starting (a corked stream) leaves the rest to the tick.
                // A skipped probe is the use mark's widest window (a
                // level meter's run, then an app, seen together a tick
                // later): usemark cuts there unless only apps record.
                let due = run_probe_at.is_none_or(|t| t.elapsed() >= RUN_PROBE_EVERY);
                if cfg.enabled && watched.using.is_empty() && due {
                    run_probe_at = Some(Instant::now());
                    if graph_probe.is_some() {
                        graph_again = true;
                    } else {
                        start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
                    }
                }
            }
            Event::HealerLook(due) => {
                if healer_look == Some(due) {
                    healer_look = None;
                }
                if graph_probe.is_some() {
                    graph_again = true;
                } else {
                    start_graph_probe(&tx, &mut graph_probe, controller.default_landed());
                }
            }
            Event::Linked(done) => {
                if done.generation == hushmic::diagnostics::engine_generation() {
                    match done.outcome {
                        hushmic::link::Outcome::Linked => {
                            watched.lifecycle.observe(done.generation, Some(true));
                            last_node_present = Some(true);
                            // The chain feeds hushmic_source: the default
                            // may be taken now.
                            if cfg.enabled {
                                controller.ensure_default_takeover(&cfg, true);
                            }
                        }
                        hushmic::link::Outcome::Failed(why) => {
                            if link_failure_logged != done.generation {
                                link_failure_logged = done.generation;
                                eprintln!(
                                    "[hushmic] could not link the chain into hushmic_source: \
                                     {why}"
                                );
                            }
                        }
                        // Waiting for the session manager: the tick asks
                        // again while both ends are up and unlinked.
                        hushmic::link::Outcome::NoPorts | hushmic::link::Outcome::Superseded => {}
                    }
                }
            }
            Event::WindowSettled(pid) => {
                if let Some(c) = companion.as_mut().filter(|c| c.child.id() == pid) {
                    c.settling = false;
                }
                sync_window_shows(&companion);
            }
            Event::CloseNoticeDue(session_stays) => {
                if first_close.due(session_stays, companion.is_some(), notify::enabled()) {
                    notify::send_transient(
                        Slot::Notice,
                        "audio-input-microphone",
                        &tr!("notify-closed-summary"),
                        &tr!("notify-closed-body"),
                    );
                    cfg.close_notice_sent = true;
                    saver.save(&cfg);
                }
            }
            Event::Unwritten(e) => {
                eprintln!("[hushmic] {e}");
                notify::send_transient(
                    Slot::Notice,
                    "dialog-error",
                    &tr!("notify-settings-unsaved-summary"),
                    &e,
                );
            }
            Event::Shutdown => {
                // SIGTERM/SIGINT/SIGHUP: restore the default mic + reap the
                // child, then leave the loop. A first close just before
                // was part of it (a logout): no notice.
                first_close.shutdown();
                let _ = controller.disable();
                break;
            }
            // consumed by the preprocessing above (answered inline or
            // rewritten into a synthetic SetMode command)
            Event::Control(_) => unreachable!("control requests are preprocessed"),
            Event::Shortcut(_) => unreachable!("shortcut events are preprocessed"),
        }
    }
    // Quit, Turn off and the session's end stop both chains; the
    // microphone's went before the loop ended.
    #[cfg(feature = "speakers")]
    speakers.stop();
    // Watchers hear that this was a quit, not a crash (UX panel round 2,
    // A10), before the window and the socket go.
    hub.stop(Duration::from_millis(500));
    // The window would die with us anyway.
    close_companion(&mut companion);
    // The file workers would too (PDEATHSIG); their caches would not.
    jobs.shutdown();
    // Let go of the global shortcut keys before the process goes.
    shortcuts::shutdown(&shortcuts_cmd, std::time::Duration::from_secs(2));
    // Orderly shutdown removes the control socket; a crash leaves it for
    // the next start's unlink+rebind (and clients' connects fail = exit 2).
    let _ = std::fs::remove_file(&control_socket_path);
    // and the show socket: only the lock holder binds it (UX panel round 2,
    // A11; the next start rebinds over one a crash left)
    let _ = std::fs::remove_file(lock::default_show_socket_path());
    // Quit/Shutdown may interrupt a running mic test: its worker dies with
    // the process (recorders via PDEATHSIG) before its own cleanup runs —
    // the voice recordings must not outlive the app. (Unlinking files the
    // recorders still hold open is fine: the data dies with their fds.)
    mictest::remove_recordings();
    hushmic::miccheck::remove_files();
    // the chain is gone: so is its /dev/shm directory, when it used one
    hushmic::meter::leave();
    // Quit or SIGTERM: a change made just before is written before the
    // process (and its single-instance lock) goes.
    drop(saver);
}

#[cfg(test)]
mod tests {
    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    /// A107 SF-N1: only "Turn on" turns HushMic on; the bubble opens the
    /// window.
    #[test]
    fn a_notifications_bubble_opens_the_window_its_button_acts() {
        use super::{notice_command, notify::Slot, RunMode, TrayCmd};
        assert!(matches!(
            notice_command(Slot::Power, "turn-on"),
            Some(TrayCmd::SetMode(Some(RunMode::Suppress)))
        ));
        for k in ["default", "", "anything"] {
            assert!(
                matches!(notice_command(Slot::Power, k), Some(TrayCmd::Open)),
                "{k}"
            );
            assert!(
                matches!(notice_command(Slot::Silence, k), Some(TrayCmd::Open)),
                "{k}"
            );
        }
        assert!(matches!(
            notice_command(Slot::Silence, "use-mic:alsa_input.usb"),
            Some(TrayCmd::SelectMic(Some(m))) if m == "alsa_input.usb"
        ));
        assert!(notice_command(Slot::Files, "default").is_none());
    }

    #[test]
    fn help_is_asked_for_in_every_usual_way() {
        use super::{help_request, usage_text, Help};
        assert_eq!(help_request(&words(&[])), Help::None);
        for h in [&["--help"][..], &["-h"], &["help"], &["--tray", "--help"]] {
            assert_eq!(help_request(&words(h)), Help::Text(usage_text()), "{h:?}");
        }
        let Help::Text(t) = help_request(&words(&["config", "get", "mic", "-h"])) else {
            panic!("config help");
        };
        // its own forms, keys with their values, its own exit codes (F12)
        assert!(t.starts_with("Usage: hushmic config [--json]"), "{t}");
        // the values line up after the longest key
        let attn = if cfg!(feature = "speakers") {
            "  attn_limit           0 to 100 (dB)"
        } else {
            "  attn_limit     0 to 100 (dB)"
        };
        assert!(t.contains(attn) && !t.contains("hushmic status"), "{t}");
        assert!(!t.contains("already running"), "{t}");
        let Help::Text(t) = help_request(&words(&["status", "--help"])) else {
            panic!("status help");
        };
        assert!(t.starts_with("Usage: hushmic status [--json]\n"), "{t}");
        assert!(
            t.contains("2 HushMic is\nnot running") && !t.contains("3 "),
            "{t}"
        );
        // clean's own help, wherever --help is among its words
        let Help::Text(t) = help_request(&words(&["clean", "a.wav", "--help"])) else {
            panic!("clean help");
        };
        assert!(t.starts_with("Usage: hushmic clean"), "{t}");
        // a command without --help runs; the window's own requests have none
        assert_eq!(help_request(&words(&["status", "--json"])), Help::None);
        assert_eq!(help_request(&words(&["files", "--help"])), Help::None);
        assert!(matches!(
            help_request(&words(&["help", "nope"])),
            Help::Unknown(_)
        ));
    }

    #[test]
    fn flags_say_what_is_wrong_in_one_line() {
        use super::flags_problem;
        let known = ["--tray", "--headless", "--doctor"];
        assert_eq!(flags_problem(&words(&["--tray"]), &known), None);
        assert_eq!(flags_problem(&words(&["--tray", "--tray"]), &known), None);
        assert_eq!(
            flags_problem(&words(&["--tary"]), &known).as_deref(),
            Some("hushmic: unknown option '--tary' (see hushmic --help)")
        );
        assert_eq!(
            flags_problem(&words(&["stauts"]), &known).as_deref(),
            Some("hushmic: unknown command 'stauts' (see hushmic --help)")
        );
        assert_eq!(
            flags_problem(&words(&["--tray", "--doctor", "--tray"]), &known).as_deref(),
            Some("hushmic: --tray and --doctor can't be used together (one at a time)")
        );
    }

    #[test]
    fn retry_says_what_it_does() {
        use super::retry_reply;
        assert_eq!(retry_reply(true, true), "retrying");
        assert!(retry_reply(true, false).starts_with("nothing is down"));
        assert!(retry_reply(false, true).starts_with("HushMic is off"));
    }

    /// Review N-M2: no silent-mic notice without an app recording.
    /// UX panel round 2, A5: "No sound from …" offers the one other
    /// microphone there is, and none among several.
    #[test]
    fn a_silent_mic_offers_the_one_other_mic() {
        use super::{pipewire, silent_mic_way_out};
        let src = |n: &str| pipewire::Source {
            name: n.into(),
            description: n.into(),
        };
        let two = [src("jabra"), src("laptop")];
        assert_eq!(
            silent_mic_way_out(&two, Some("jabra")).map(|s| s.name.as_str()),
            Some("laptop")
        );
        assert!(silent_mic_way_out(&two[..1], Some("jabra")).is_none());
        let three = [src("jabra"), src("laptop"), src("webcam")];
        assert!(silent_mic_way_out(&three, Some("jabra")).is_none());
        // the silent one unknown: two others are no single way out
        assert!(silent_mic_way_out(&two, None).is_none());
    }

    /// UX panel round 2, A9: the fallback starts with the settings of the
    /// microphone the session manager already moved the capture to.
    #[test]
    fn the_fallback_stands_in_with_the_mic_already_linked() {
        use super::stand_in;
        assert_eq!(
            stand_in(Some("headset"), Some("usbmic")).as_deref(),
            Some("usbmic")
        );
        // never the chosen one, nor our own node or a monitor
        assert!(stand_in(Some("headset"), Some("headset")).is_none());
        assert!(stand_in(Some("headset"), Some("hushmic_source")).is_none());
        assert!(stand_in(Some("headset"), Some("sink.monitor")).is_none());
        assert!(stand_in(Some("headset"), None).is_none());
    }

    #[test]
    fn a_silent_mic_needs_an_app_and_a_running_chain() {
        use super::mic_silent_now;
        assert!(mic_silent_now(true, true, || true));
        assert!(!mic_silent_now(true, false, || true), "no app records");
        assert!(!mic_silent_now(false, true, || true), "not running");
        assert!(!mic_silent_now(true, true, || false));
        // the meter is read only when it can matter
        assert!(!mic_silent_now(true, false, || panic!("read")));
    }

    /// Review N-M1: while either question shows, the tray's Quit or Off
    /// brings it to the front instead of asking again or quitting.
    #[test]
    fn a_tray_quit_while_a_question_shows_brings_it_forward() {
        use super::tray_question_up;
        use hushmic::appstate::Ask;
        assert!(!tray_question_up(false, None));
        assert!(tray_question_up(true, None), "the files question");
        assert!(tray_question_up(false, Some(Ask::Quit)));
        assert!(tray_question_up(false, Some(Ask::TurnOff)));
    }

    #[test]
    fn the_trays_quit_asks_once_then_quits() {
        use super::{tray_quit, TrayQuit};
        assert_eq!(tray_quit(0, false), TrayQuit::Quit, "nothing to stop");
        assert_eq!(tray_quit(2, false), TrayQuit::Ask);
        assert_eq!(tray_quit(2, true), TrayQuit::Quit, "a second Quit");
        assert_eq!(tray_quit(0, true), TrayQuit::Quit, "finished while asking");
    }

    /// UX panel A3: the tray's Quit and Off ask first while an app uses
    /// HushMic (Off: while it is on); a second one while it asks goes
    /// ahead.
    #[test]
    fn the_trays_quit_and_off_ask_while_an_app_uses_hushmic() {
        use super::{tray_asks, TrayCmd};
        use hushmic::appstate::Ask;
        for (cmd, ask) in [
            (TrayCmd::AskQuit, Ask::Quit),
            (TrayCmd::AskOff, Ask::TurnOff),
        ] {
            assert!(tray_asks(&cmd, true, None, true), "{ask:?}");
            assert!(!tray_asks(&cmd, false, None, true), "{ask:?}: no app");
            assert!(
                !tray_asks(&cmd, true, Some(ask), true),
                "{ask:?}: a second one"
            );
        }
        // Off while off: nothing to ask; Quit while off still asks
        assert!(!tray_asks(&TrayCmd::AskOff, true, None, false));
        assert!(tray_asks(&TrayCmd::AskQuit, true, None, false));
        // the other question up: this one is asked instead
        assert!(tray_asks(&TrayCmd::AskQuit, true, Some(Ask::TurnOff), true));
        assert!(!tray_asks(&TrayCmd::Quit, true, None, true));
    }

    #[test]
    fn a_window_closed_without_an_answer_withdraws_the_quit_question() {
        use super::{quit_question_after, tray_quit, QuitQuestion, TrayQuit};
        for end in [WindowEnd::Closed, WindowEnd::Died] {
            assert_eq!(quit_question_after(&end, true), QuitQuestion::Withdraw);
            assert_eq!(quit_question_after(&end, false), QuitQuestion::Keep);
        }
        assert_eq!(
            quit_question_after(&WindowEnd::CouldNotOpen, true),
            QuitQuestion::Quit
        );
        assert_eq!(
            quit_question_after(&WindowEnd::CouldNotOpen, false),
            QuitQuestion::Keep
        );
        // withdrawn, the next Quit asks again; shown, it quits
        assert_eq!(tray_quit(2, false), TrayQuit::Ask);
    }

    use super::{on_live, window_end, window_mask, EngineCache, EngineView, Shown, WindowEnd};
    use hushmic::diagnostics::EngineTier;

    #[test]
    fn back_after_a_restart_counts_once_per_outage() {
        use hushmic::appstate::Phase::*;
        let run = |steps: &[(hushmic::appstate::Phase, u64)]| -> Vec<bool> {
            let mut shown = Shown::default();
            steps
                .iter()
                .map(|&(p, n)| shown.back_after_restart(p, n))
                .collect()
        };
        // a launch, then a routine reconnect: once, at Ready
        assert_eq!(
            run(&[
                (Starting, 0),
                (Ready, 0),
                (Reconnecting, 0),
                (Ready, 0),
                (Ready, 0)
            ]),
            [false, false, false, true, false]
        );
        // a failure on the way (a respawn that failed): its recovery was
        // counted when it came up, so Ready after Reconnecting is not
        // counted again (seen on a private daemon)
        assert_eq!(
            run(&[(Ready, 1), (Down, 1), (Reconnecting, 2), (Ready, 2)]),
            [false, false, false, false]
        );
        // a start, a failure, a user's turn-on: never a reconnect
        assert_eq!(
            run(&[(Off, 0), (Starting, 0), (Ready, 0), (Down, 0), (Ready, 1)]),
            [false, false, false, false, false]
        );
        // two outages: once each
        assert_eq!(
            run(&[
                (Ready, 0),
                (Reconnecting, 0),
                (Ready, 0),
                (Reconnecting, 1),
                (Ready, 1)
            ]),
            [false, false, true, false, true]
        );
    }

    #[test]
    fn the_fallback_is_what_the_capture_link_says() {
        // the recovery moved the chain to the default: a fallback
        assert!(super::fallen_back("usb", None, Some("laptop"), false));
        assert!(
            super::fallen_back("usb", None, None, false),
            "no graph read yet"
        );
        // replugged and relinked before the recovery's restart: over
        assert!(!super::fallen_back("usb", None, Some("usb"), true));
        // unplugged, WirePlumber moved the pinned chain before the
        // recovery acted: a fallback already
        assert!(super::fallen_back(
            "usb",
            Some("usb"),
            Some("laptop"),
            false
        ));
        // on the chosen mic, or a present one another tool moved away
        // from (rerouted, not unplugged)
        assert!(!super::fallen_back("usb", Some("usb"), Some("usb"), true));
        assert!(!super::fallen_back("usb", Some("usb"), None, true));
        assert!(!super::fallen_back(
            "usb",
            Some("usb"),
            Some("laptop"),
            true
        ));
    }

    #[test]
    fn a_new_window_shows_its_page_until_it_settles() {
        use hushmic::appstate::Page;
        use hushmic::notify::{LiveShows, Problem};
        let untold = LiveShows::Untold;
        let live = hushmic::notify::shown_on(Page::Live, untold);
        // settling (just opened, no focus yet): as if it had focus
        assert_eq!(window_mask(false, true, Page::Live, untold), live);
        assert_eq!(window_mask(true, false, Page::Live, untold), live);
        // no focus once settled: shows nothing
        assert_eq!(window_mask(false, false, Page::Live, untold), 0);
        // the page still decides
        assert_eq!(window_mask(false, true, Page::Files, untold), 0);
        // once told, Live claims only its Status card's problem
        let one = LiveShows::One(Problem::Rerouted);
        assert_eq!(
            window_mask(true, false, Page::Live, one),
            hushmic::notify::shown_on(Page::Live, one)
        );
        assert_ne!(window_mask(true, false, Page::Live, one), live);
        // Live in front waits for its card's word; elsewhere nothing waits
        assert!(on_live(true, false, Page::Live));
        assert!(on_live(false, true, Page::Live));
        assert!(!on_live(false, false, Page::Live));
        assert!(!on_live(true, true, Page::Files));
        assert!(!on_live(true, false, Page::Settings));
    }

    #[test]
    fn only_a_window_that_never_came_up_could_not_open() {
        use std::os::unix::process::ExitStatusExt;
        let code = |c: i32| std::process::ExitStatus::from_raw(c << 8);
        assert_eq!(window_end(code(0)), WindowEnd::Closed);
        assert_eq!(
            window_end(code(hushmic::window::EXIT_COULD_NOT_OPEN)),
            WindowEnd::CouldNotOpen
        );
        // its display went away, a panic, a signal: ran, then died
        assert_eq!(
            window_end(code(hushmic::window::EXIT_DISPLAY_GONE)),
            WindowEnd::Died
        );
        assert_eq!(window_end(code(101)), WindowEnd::Died);
        assert_eq!(
            window_end(std::process::ExitStatus::from_raw(libc::SIGKILL)),
            WindowEnd::Died
        );
    }

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
