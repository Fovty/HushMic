//! Best-effort desktop notifications (`org.freedesktop.Notifications`).
//!
//! Errors that only reach stderr are invisible for .desktop/autostart
//! launches, so user-actionable failures (enable() preflight, a stuck or
//! flapping watchdog respawn loop) are surfaced as desktop notifications
//! too. The DBus call goes through zbus directly — no dependency on a
//! `notify-send` binary being installed. Everything here is best-effort: a
//! missing, broken, or hung session bus must never break or block the mic
//! path.
//!
//! All sends go through ONE queue drained by a dedicated worker thread:
//! that guarantees delivery order (a recovery notice can never be overtaken
//! by the failure it clears), makes the per-slot replace-id chain race-free
//! by construction, and isolates every caller from a stalled bus (the
//! connection also carries a method timeout, so the queue itself cannot
//! wedge forever).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Independent replace-chains, one per topic: a new notification replaces
/// the previous one of the same slot (one evolving bubble instead of a
/// pile), and one topic never swallows another's (UX panel round 2, A5:
/// "HushMic is off" stays when a microphone leaves).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Other messages about the desktop (a denied autostart, a window
    /// that could not open).
    Status = 0,
    /// `hushmic miccheck` progress and results.
    MicTest = 1,
    /// Information about the app itself (the first close of the window).
    Notice = 2,
    /// The virtual microphone failing (could not start, stuck, keeps
    /// restarting) and the "running again" that closes it: a short-lived
    /// microphone notice must never replace such a bubble.
    Chain = 3,
    /// The Files page: recordings cleaned while the window was away.
    Files = 4,
    /// The microphone in use: the chosen one gone or back, the default
    /// moved to another.
    Mic = 5,
    /// No sound from the microphone (closed when sound is back).
    Silence = 6,
    /// HushMic off or quit while apps used it.
    Power = 7,
    /// The default microphone moved away from HushMic.
    Default = 8,
    /// Another app takes the capture, or the microphone does not connect.
    Route = 9,
    /// HushMic Speakers failing to start, and running again after that.
    #[cfg(feature = "speakers")]
    Speakers = 10,
}

/// A hung notification server (on GNOME it is the shell itself) must not
/// stall the queue worker indefinitely; real servers reply in milliseconds.
const METHOD_TIMEOUT: Duration = Duration::from_secs(5);

static LAST_ID: [AtomicU32; SLOTS.len()] = [const { AtomicU32::new(0) }; SLOTS.len()];

/// The slots in `LAST_ID` order.
const SLOTS: [Slot; 10 + cfg!(feature = "speakers") as usize] = [
    Slot::Status,
    Slot::MicTest,
    Slot::Notice,
    Slot::Chain,
    Slot::Files,
    Slot::Mic,
    Slot::Silence,
    Slot::Power,
    Slot::Default,
    Slot::Route,
    #[cfg(feature = "speakers")]
    Slot::Speakers,
];

/// Who hears a click on a notification's action: the slot of the
/// notification and the action's key ("default" for the bubble itself).
type ActionHandler = Box<dyn Fn(Slot, String) + Send + Sync>;
static ON_ACTION: OnceLock<ActionHandler> = OnceLock::new();

/// Hear the actions of HushMic's notifications from now on (once per
/// process; a second handler is ignored). Without one, notifications are
/// sent without actions.
pub fn on_action(f: impl Fn(Slot, String) + Send + Sync + 'static) {
    let _ = ON_ACTION.set(Box::new(f));
    // A connection made before (a notification sent earlier) is heard too.
    let mut c = CONN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(link) = c.as_mut() {
        link.hear();
    }
}

/// Process-global switch for the `notifications` config key. Read at
/// enqueue time so a `config set notifications false` silences the very
/// next send.
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The problems the window shows right now: the ones its page shows, while
/// it has keyboard focus; none otherwise. Such a problem is held back
/// instead of also becoming a desktop notification. The daemon's loop
/// keeps it ([`shown_on`]).
static WINDOW_SHOWS: AtomicU16 = AtomicU16::new(0);

pub fn set_window_shows(problems: u16) {
    WINDOW_SHOWS.store(problems, Ordering::Relaxed);
}

/// The window shows Live (with focus, or settling): a problem its Status
/// card could show that arrives now is held for [`CARD_GRACE`] even if the
/// card shows none yet. The daemon decides at arrival, before the window
/// has seen the state that carries the problem and told what its card
/// shows (`ui shows`); after the grace the card's word decides.
static WINDOW_ON_LIVE: AtomicBool = AtomicBool::new(false);

pub fn set_window_on_live(on: bool) {
    WINDOW_ON_LIVE.store(on, Ordering::Relaxed);
}

/// How long a problem arriving while Live shows waits for the window's
/// word on its Status card.
pub const CARD_GRACE: Duration = Duration::from_secs(1);

/// What the daemon's loop is asked to do when a grace runs out: judge the
/// held problems again ([`release_held`]).
type Wake = Box<dyn Fn() + Send + Sync>;

fn wake_slot() -> &'static Mutex<Option<Wake>> {
    static WAKE: OnceLock<Mutex<Option<Wake>>> = OnceLock::new();
    WAKE.get_or_init(|| Mutex::new(None))
}

/// The daemon's loop: called (from a timer thread) when a problem's grace
/// on Live runs out.
pub fn set_wake(wake: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut w) = wake_slot().lock() {
        *w = Some(Box::new(wake));
    }
}

pub fn window_shows(problem: Problem) -> bool {
    WINDOW_SHOWS.load(Ordering::Relaxed) & problem.bit() != 0
}

/// What Live's Status card shows, as the window last told the daemon
/// (`ui shows …`): it shows one problem at a time, so one it can't show
/// must still become a notification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LiveShows {
    /// A new window that hasn't said yet: any it could show (a launch's
    /// failure must not beat the window it is about to show in).
    #[default]
    Untold,
    /// The problem its Status card shows.
    One(Problem),
    /// None of these (a quieter one, or nothing wrong).
    Nothing,
}

/// The problems `page` shows, for [`set_window_shows`].
/// - Live: the one its Status card shows ([`LiveShows`]), and the moments
///   its title and picture say ("Running again.", the microphone's name
///   back on the picture).
/// - First run: the chain down or stuck, as its takeover.
/// - Settings: the default released (the note under Settings › Apps).
/// - Files, Shortcuts, Speakers and About: none.
pub fn shown_on(page: crate::appstate::Page, live: LiveShows) -> u16 {
    use crate::appstate::Page;
    match page {
        Page::Live => {
            let card = match live {
                LiveShows::Untold => Problem::ALL
                    .iter()
                    .filter(|p| **p != Problem::DefaultReleased)
                    .fold(0, |m, p| m | p.bit()),
                LiveShows::One(p) => p.bit(),
                LiveShows::Nothing => 0,
            };
            card | Problem::Recovered.bit() | Problem::MicBack.bit()
        }
        Page::FirstRun => Problem::Failure.bit(),
        Page::Settings => Problem::DefaultReleased.bit(),
        Page::Files | Page::Shortcuts | Page::About => 0,
        #[cfg(feature = "speakers")]
        Page::Speakers => 0,
    }
}

/// What a problem notification is about, so one held back while the window
/// had focus can be judged again when focus goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The chain could not start, or keeps failing to.
    Failure,
    /// The chain keeps restarting.
    Flapping,
    /// Running again after a failure.
    Recovered,
    /// Another app keeps taking the capture stream.
    Rerouted,
    /// The chosen microphone is gone; the default stands in.
    MicFallback,
    /// The chosen microphone is back.
    MicBack,
    /// The user moved the default away from HushMic.
    DefaultReleased,
    /// The capture stream stays fed by nothing though chain restarts
    /// tried: the microphone does not connect.
    MicStuck,
    /// An app uses HushMic and the microphone has sent exact digital
    /// silence for 10 s (a hardware mute switch, a dead cable).
    MicSilent,
}

impl Problem {
    pub const ALL: [Problem; 9] = [
        Problem::Failure,
        Problem::Flapping,
        Problem::Recovered,
        Problem::Rerouted,
        Problem::MicFallback,
        Problem::MicBack,
        Problem::DefaultReleased,
        Problem::MicStuck,
        Problem::MicSilent,
    ];

    fn bit(self) -> u16 {
        1 << self as u16
    }

    /// Its word in `ui shows …`: the problems Live's Status card can show.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Problem::Failure => Some("failure"),
            Problem::Flapping => Some("flapping"),
            Problem::Rerouted => Some("rerouted"),
            Problem::MicFallback => Some("mic-fallback"),
            Problem::MicStuck => Some("mic-stuck"),
            Problem::MicSilent => Some("mic-silent"),
            Problem::Recovered | Problem::MicBack | Problem::DefaultReleased => None,
        }
    }

    pub fn parse(word: &str) -> Option<Problem> {
        Problem::ALL.into_iter().find(|p| p.word() == Some(word))
    }

    /// The replace-chain its notifications use.
    pub fn slot(self) -> Slot {
        match self {
            Problem::Failure | Problem::Flapping | Problem::Recovered => Slot::Chain,
            Problem::MicFallback | Problem::MicBack => Slot::Mic,
            Problem::MicSilent => Slot::Silence,
            Problem::DefaultReleased => Slot::Default,
            Problem::Rerouted | Problem::MicStuck => Slot::Route,
        }
    }

    /// A moment rather than a state: once it has passed, there is nothing
    /// left to tell.
    pub fn passes(self) -> bool {
        matches!(self, Problem::Recovered | Problem::MicBack)
    }
}

/// A notification as queued.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub slot: Slot,
    pub icon: String,
    pub summary: String,
    pub body: String,
    pub transient: bool,
    /// (key, label) pairs, as [`send_with_actions`] takes them.
    pub actions: Vec<(String, String)>,
}

/// The problem notifications the window may show: the ones held back while
/// it showed them, and whether a failure bubble is up. Holding one is not
/// sending it: once the window no longer shows it (focus went, another
/// page, closed), it is sent if still so, once.
#[derive(Default)]
pub struct Problems {
    held: Vec<Held>,
    /// A failure (or flapping) bubble went out and no "running again" has
    /// replaced it yet.
    failure_up: bool,
}

/// A held problem notification; `until`: held only for Live's grace
/// (the window didn't show it when it came).
struct Held {
    problem: Problem,
    note: Note,
    until: Option<Instant>,
}

impl Problems {
    /// A problem notification arrives while the window shows it (`shown`)
    /// or not. Returns the note to send now. "Running again" only ever
    /// replaces a failure bubble, shown or not: with none up (the failure
    /// was only in the window, or over before it was told) there is
    /// nothing to close, and the window says it itself.
    pub fn arrive(&mut self, problem: Problem, note: Note, shown: bool) -> Option<Note> {
        self.arrive_until(problem, note, shown, None)
    }

    /// As [`Problems::arrive`], and one the window doesn't show is held
    /// all the same until `grace` (Live, whose Status card hasn't seen it
    /// yet: [`CARD_GRACE`]).
    pub fn arrive_until(
        &mut self,
        problem: Problem,
        note: Note,
        shown: bool,
        grace: Option<Instant>,
    ) -> Option<Note> {
        if problem == Problem::Recovered {
            return self.failure_up.then(|| self.sent(problem, note));
        }
        if shown || grace.is_some() {
            let until = if shown { None } else { grace };
            match self.held.iter_mut().find(|h| h.problem == problem) {
                // a newer one about the same problem keeps the older's place
                Some(held) => {
                    held.note = note;
                    held.until = held.until.max(until);
                }
                None => self.held.push(Held {
                    problem,
                    note,
                    until,
                }),
            }
            return None;
        }
        Some(self.sent(problem, note))
    }

    fn sent(&mut self, problem: Problem, note: Note) -> Note {
        match problem {
            Problem::Failure | Problem::Flapping => self.failure_up = true,
            Problem::Recovered => self.failure_up = false,
            _ => {}
        }
        note
    }

    /// The window shows only `shows` now: of the held notifications whose
    /// problem it no longer shows, the ones still so (`still`; a moment
    /// never is) to send, short-lived ones first so that a lasting one is
    /// what stays on screen, and the problems dropped. The ones it still
    /// shows stay held. With notifications off nothing is sent.
    pub fn release(
        &mut self,
        shows: u16,
        still: impl Fn(Problem) -> bool,
        notifications: bool,
    ) -> (Vec<Note>, Vec<Problem>) {
        self.release_at(shows, None, still, notifications)
    }

    /// As [`Problems::release`]; `live`: the window still shows Live, at
    /// this instant, so one held in its grace stays until the grace ends.
    pub fn release_at(
        &mut self,
        shows: u16,
        live: Option<Instant>,
        still: impl Fn(Problem) -> bool,
        notifications: bool,
    ) -> (Vec<Note>, Vec<Problem>) {
        let mut send = Vec::new();
        let mut dropped = Vec::new();
        for h in std::mem::take(&mut self.held) {
            let p = h.problem;
            let waiting = live.is_some_and(|now| h.until.is_some_and(|u| now < u));
            if shows & p.bit() != 0 || waiting {
                self.held.push(h);
            } else if notifications && !p.passes() && still(p) {
                send.push(self.sent(p, h.note));
            } else {
                dropped.push(p);
            }
        }
        send.sort_by_key(|n| !n.transient);
        (send, dropped)
    }

    /// The user turned HushMic off: a failure bubble from before is not
    /// one a later "running again" answers.
    pub fn forget_failure(&mut self) {
        self.failure_up = false;
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

fn problems() -> &'static Mutex<Problems> {
    static PROBLEMS: OnceLock<Mutex<Problems>> = OnceLock::new();
    PROBLEMS.get_or_init(|| Mutex::new(Problems::default()))
}

fn enqueue_note(n: Note) {
    enqueue(Msg {
        slot: n.slot,
        icon: n.icon,
        summary: n.summary,
        body: n.body,
        transient: n.transient,
        actions: with_default(&n.actions),
        ack: None,
        close: false,
    });
}

/// A notification's actions as sent: none without anyone to hear a click,
/// else the bubble itself ("default") too.
fn with_default(actions: &[(String, String)]) -> Vec<(String, String)> {
    if actions.is_empty() || ON_ACTION.get().is_none() {
        return Vec::new();
    }
    std::iter::once(("default".to_string(), String::new()))
        .chain(actions.iter().cloned())
        .collect()
}

/// Send the held problems the window no longer shows that are still so,
/// once, and forget the others it no longer shows. Returns the problems
/// dropped.
pub fn release_held(still: impl Fn(Problem) -> bool) -> Vec<Problem> {
    let (send, dropped) = match problems().lock() {
        Ok(mut h) if !h.is_empty() => {
            let live = WINDOW_ON_LIVE.load(Ordering::Relaxed).then(Instant::now);
            h.release_at(WINDOW_SHOWS.load(Ordering::Relaxed), live, still, enabled())
        }
        _ => return Vec::new(),
    };
    send.into_iter().for_each(enqueue_note);
    dropped
}

/// See [`Problems::forget_failure`].
pub fn forget_failure_bubble() {
    if let Ok(mut h) = problems().lock() {
        h.forget_failure();
    }
}

fn problem(problem: Problem, note: Note) {
    if !enabled() {
        return;
    }
    // Live may not have seen it yet: a problem its Status card could show
    // waits for the card's word (then a timer has the loop judge again)
    let shown = window_shows(problem);
    let grace = (!shown && problem.word().is_some() && WINDOW_ON_LIVE.load(Ordering::Relaxed))
        .then(|| Instant::now() + CARD_GRACE);
    let now = match problems().lock() {
        Ok(mut h) => h.arrive_until(problem, note, shown, grace),
        Err(_) => Some(note),
    };
    if grace.is_some() && now.is_none() {
        let _ = std::thread::Builder::new()
            .name("hushmic-grace".into())
            .spawn(|| {
                std::thread::sleep(CARD_GRACE + Duration::from_millis(50));
                if let Ok(w) = wake_slot().lock() {
                    if let Some(w) = w.as_ref() {
                        w();
                    }
                }
            });
    }
    if let Some(note) = now {
        enqueue_note(note);
    }
}

/// How long the first-close notice waits when there is no session to watch
/// (no logind): a close that belongs to a logout (the session manager
/// closes the windows, then ends the daemon) must not spend it.
pub const CLOSE_NOTICE_DELAY: Duration = Duration::from_secs(3);
/// How long the session the window was in is watched after its close
/// before the first-close notice goes out.
pub const CLOSE_NOTICE_WATCH: Duration = Duration::from_secs(10);
const WATCH_STEP: Duration = Duration::from_millis(500);

/// The one-time "HushMic is still running" notice for the user's first
/// close of the window, sent once the session the window was in has been
/// watched ([`session_stays`]): a shutdown meanwhile (the loop ends), a
/// session closing, gone or no longer active, or a window open again sends
/// nothing and leaves it unsent for a later close.
#[derive(Default)]
pub struct FirstClose {
    pending: bool,
}

impl FirstClose {
    /// The user closed the window. True: watch the session, then wake the
    /// loop.
    pub fn closed(&mut self, already_sent: bool, notifications: bool) -> bool {
        if already_sent || !notifications || self.pending {
            return false;
        }
        self.pending = true;
        true
    }

    /// The daemon is shutting down: a close just before was part of it.
    pub fn shutdown(&mut self) {
        self.pending = false;
    }

    /// The watch is over (`session_stays`, [`session_stays`]'s word;
    /// `window_open`: the user opened it again meanwhile). True: send the
    /// notice now, and remember that it was.
    pub fn due(&mut self, session_stays: bool, window_open: bool, notifications: bool) -> bool {
        std::mem::take(&mut self.pending) && session_stays && !window_open && notifications
    }
}

fn system_bus() -> zbus::Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::system()?
        .method_timeout(METHOD_TIMEOUT)
        .build()
}

/// The logind session the window opens in: this process's, else (a daemon
/// outside any session: an app scope under the user manager, XDG
/// autostart, the systemd user unit) the user's display session. None
/// without logind or such a session.
pub fn display_session() -> Option<String> {
    fn ask() -> zbus::Result<Option<String>> {
        use zbus::zvariant::OwnedObjectPath;
        let conn = system_bus()?;
        let manager = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )?;
        let own: zbus::Result<OwnedObjectPath> =
            manager.call("GetSessionByPID", &(std::process::id()));
        if let Ok(path) = own {
            return Ok(Some(path.to_string()));
        }
        let uid = unsafe { libc::getuid() };
        let user: OwnedObjectPath = manager.call("GetUser", &(uid))?;
        let user = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            user,
            "org.freedesktop.login1.User",
        )?;
        let (_, display): (String, OwnedObjectPath) = user.get_property("Display")?;
        Ok((display.as_str() != "/").then(|| display.to_string()))
    }
    ask().ok().flatten()
}

/// One look at a session: its State and whether it is Active; None once
/// logind no longer has it, or the desktop says it is ending.
type SessionLook = Option<(String, bool)>;

/// KDE's session manager, on the host's session bus (a sandbox cannot see
/// it): it says when it is logging out, which is when KWin closes the
/// windows, before logind hears of it.
pub struct Ksm {
    conn: zbus::blocking::Connection,
    bus: zbus::blocking::fdo::DBusProxy<'static>,
    /// It answered once: going quiet after that is part of a logout.
    answered: bool,
}

impl Ksm {
    pub fn connect() -> Option<Ksm> {
        let conn = zbus::blocking::connection::Builder::session()
            .ok()?
            .method_timeout(Duration::from_secs(2))
            .build()
            .ok()?;
        let bus = zbus::blocking::fdo::DBusProxy::new(&conn).ok()?;
        Some(Ksm {
            conn,
            bus,
            answered: false,
        })
    }

    /// It is logging out, or went quiet after it had answered. Asked only
    /// while it is there: a call must never start a session manager.
    pub fn ending(&mut self) -> bool {
        let there = zbus::names::BusName::try_from("org.kde.ksmserver")
            .ok()
            .and_then(|name| self.bus.name_has_owner(name).ok())
            .unwrap_or(false);
        if !there {
            return self.answered;
        }
        let reply = self
            .conn
            .call_method(
                Some("org.kde.ksmserver"),
                "/KSMServer",
                Some("org.kde.KSMServerInterface"),
                "isShuttingDown",
                &(),
            )
            .and_then(|m| m.body().deserialize::<bool>());
        match reply {
            Ok(shutting_down) => {
                self.answered = true;
                shutting_down
            }
            Err(_) => self.answered,
        }
    }
}

/// After the window's close, watch the session it was in for
/// [`CLOSE_NOTICE_WATCH`], and the desktop's own word: KDE's session
/// manager ([`Ksm`]) and the Inhibit portal's monitor
/// ([`crate::session_end`], which a Flatpak can reach). False as soon as
/// one says the session is closing, gone or ending (a logout), else whether
/// it is still active at the end. With none to watch, the portal's word
/// after [`CLOSE_NOTICE_DELAY`]. Blocks: run it off the loop.
pub fn session_stays(session: Option<String>) -> bool {
    let logind = session.and_then(|path| {
        let conn = system_bus().ok()?;
        let proxy = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy>::new(&conn)
            .destination("org.freedesktop.login1")
            .ok()?
            .path(path)
            .ok()?
            .interface("org.freedesktop.login1.Session")
            .ok()?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .ok()?;
        Some((conn, proxy))
    });
    let mut ksm = Ksm::connect();
    if ksm.as_mut().is_some_and(Ksm::ending) {
        return false;
    }
    let ksm_there = ksm.as_ref().is_some_and(|k| k.answered);
    if logind.is_none() && !crate::session_end::watching() && !ksm_there {
        std::thread::sleep(CLOSE_NOTICE_DELAY);
        return !crate::session_end::ending();
    }
    watch_session(
        || {
            if crate::session_end::ending() || ksm.as_mut().is_some_and(Ksm::ending) {
                return None;
            }
            let Some((_conn, proxy)) = &logind else {
                return Some(("active".to_string(), true));
            };
            let state: String = proxy.get_property("State").ok()?;
            let active: bool = proxy.get_property("Active").ok()?;
            Some((state, active))
        },
        std::thread::sleep,
    )
}

fn watch_session(mut look: impl FnMut() -> SessionLook, mut wait: impl FnMut(Duration)) -> bool {
    let mut waited = Duration::ZERO;
    loop {
        match look() {
            None => return false,
            Some((state, _)) if state == "closing" => return false,
            Some((_, active)) if waited >= CLOSE_NOTICE_WATCH => return active,
            Some(_) => {}
        }
        wait(WATCH_STEP);
        waited += WATCH_STEP;
    }
}

struct Msg {
    slot: Slot,
    icon: String,
    summary: String,
    body: String,
    transient: bool,
    /// (key, label) pairs.
    actions: Vec<(String, String)>,
    ack: Option<mpsc::Sender<()>>,
    /// Close the slot's last notification instead of sending one.
    close: bool,
}

fn queue() -> &'static Mutex<mpsc::Sender<Msg>> {
    static QUEUE: OnceLock<Mutex<mpsc::Sender<Msg>>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Msg>();
        std::thread::spawn(move || {
            for m in rx {
                if m.close {
                    let _ = close_blocking(m.slot);
                    continue;
                }
                let actions: Vec<(&str, &str)> = m
                    .actions
                    .iter()
                    .map(|(k, l)| (k.as_str(), l.as_str()))
                    .collect();
                let _ =
                    send_blocking_with(m.slot, &m.icon, &m.summary, &m.body, m.transient, &actions);
                if let Some(ack) = m.ack {
                    let _ = ack.send(());
                }
            }
        });
        Mutex::new(tx)
    })
}

fn enqueue(msg: Msg) {
    if !enabled() {
        // Acknowledge a bounded wait immediately instead of letting it
        // time out on a message that will never be sent.
        if let Some(ack) = msg.ack {
            let _ = ack.send(());
        }
        return;
    }
    if let Ok(tx) = queue().lock() {
        let _ = tx.send(msg);
    }
}

/// What apps that used HushMic hear after Off or Quit, from the last
/// snapshot: "Zoom now hears RODE NT-USB directly, without noise
/// removal." None while no app uses it (or HushMic is off already).
pub fn direct_after(s: &crate::appstate::AppState) -> Option<String> {
    if s.mode == crate::appstate::Mode::Off || s.apps.using.is_empty() {
        return None;
    }
    let count = s.apps.using.len();
    let apps = name_apps(s.apps.using.iter());
    // they move to the system default (the one HushMic stood in for)
    let mic = s
        .mic
        .system_default
        .as_deref()
        .and_then(|m| s.description_of(m))
        .map(str::to_string)
        .unwrap_or_else(|| crate::tr!("notify-your-mic"));
    Some(crate::tr!(
        "notify-direct-body",
        count = count,
        apps = apps,
        mic = mic
    ))
}

/// Apps by name for a sentence: "Zoom", "Zoom and Teams", "3 apps" (an
/// app with no name is "An app").
pub fn name_apps<'a>(apps: impl IntoIterator<Item = &'a crate::appstate::AppRef>) -> String {
    let anon = crate::tr!("live-app-anon");
    let names: Vec<&str> = apps
        .into_iter()
        .map(|a| a.name.as_deref().unwrap_or(anon.as_str()))
        .collect();
    match names.as_slice() {
        [one] => one.to_string(),
        [a, b] => crate::tr!(
            "notify-apps-two",
            app1 = a.to_string(),
            app2 = b.to_string()
        ),
        _ => crate::tr!("notify-apps-many", count = names.len()),
    }
}

/// Queue a persistent notification (stays in the notification center).
/// Never blocks the caller; delivery is ordered across all sends.
pub fn send(slot: Slot, icon: &str, summary: &str, body: &str) {
    enqueue(Msg {
        slot,
        icon: icon.to_string(),
        summary: summary.to_string(),
        body: body.to_string(),
        transient: false,
        actions: Vec::new(),
        ack: None,
        close: false,
    });
}

/// A problem the window may show: queued like [`send`] (on the problem's
/// own chain) unless the window shows it now, then held back
/// ([`release_held`]).
pub fn send_problem(problem_: Problem, icon: &str, summary: &str, body: &str) {
    problem(
        problem_,
        Note {
            slot: problem_.slot(),
            icon: icon.to_string(),
            summary: summary.to_string(),
            body: body.to_string(),
            transient: false,
            actions: Vec::new(),
        },
    );
}

/// [`send_problem`] with actions, (key, label) pairs: a click comes to the
/// [`on_action`] handler with the problem's slot.
pub fn send_problem_with_actions(
    problem_: Problem,
    icon: &str,
    summary: &str,
    body: &str,
    actions: &[(String, String)],
) {
    problem(
        problem_,
        Note {
            slot: problem_.slot(),
            icon: icon.to_string(),
            summary: summary.to_string(),
            body: body.to_string(),
            transient: false,
            actions: actions.to_vec(),
        },
    );
}

/// [`send_problem`], transient.
pub fn send_problem_transient(problem_: Problem, icon: &str, summary: &str, body: &str) {
    problem(
        problem_,
        Note {
            slot: problem_.slot(),
            icon: icon.to_string(),
            summary: summary.to_string(),
            body: body.to_string(),
            transient: true,
            actions: Vec::new(),
        },
    );
}

/// Queue a transient notification (progress bubbles that should not pile up
/// in the notification center after the moment has passed).
pub fn send_transient(slot: Slot, icon: &str, summary: &str, body: &str) {
    enqueue(Msg {
        slot,
        icon: icon.to_string(),
        summary: summary.to_string(),
        body: body.to_string(),
        transient: true,
        actions: Vec::new(),
        ack: None,
        close: false,
    });
}

/// [`send`] with actions, (key, label) pairs: a click comes to the
/// [`on_action`] handler. The bubble itself (its "default" action) does
/// what the first one does.
pub fn send_with_actions(
    slot: Slot,
    icon: &str,
    summary: &str,
    body: &str,
    actions: &[(&str, &str)],
) {
    let owned: Vec<(String, String)> = actions
        .iter()
        .map(|(k, l)| (k.to_string(), l.to_string()))
        .collect();
    let all = with_default(&owned);
    enqueue(Msg {
        slot,
        icon: icon.to_string(),
        summary: summary.to_string(),
        body: body.to_string(),
        transient: false,
        actions: all,
        ack: None,
        close: false,
    });
}

/// Like [`send`], but waits (bounded) for the delivery attempt. For paths
/// that exit the process right after — the queue worker dies with the
/// process and the notification would be silently lost.
pub fn send_and_wait(slot: Slot, icon: &str, summary: &str, body: &str, max: Duration) {
    let (tx, rx) = mpsc::channel();
    enqueue(Msg {
        slot,
        icon: icon.to_string(),
        summary: summary.to_string(),
        body: body.to_string(),
        transient: false,
        actions: Vec::new(),
        ack: Some(tx),
        close: false,
    });
    let _ = rx.recv_timeout(max);
}

/// Close the slot's notification, if one is out (the moment it told of is
/// over: sound is back). Never blocks the caller; ordered with the sends.
pub fn close(slot: Slot) {
    enqueue(Msg {
        slot,
        icon: String::new(),
        summary: String::new(),
        body: String::new(),
        transient: false,
        actions: Vec::new(),
        ack: None,
        close: true,
    });
}

/// `CloseNotification` for the slot's last id; the next send starts a new
/// bubble.
fn close_blocking(slot: Slot) -> zbus::Result<()> {
    let id = LAST_ID[slot as usize].swap(0, Ordering::Relaxed);
    if id == 0 {
        return Ok(());
    }
    connection()?
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "CloseNotification",
            &(id,),
        )
        .inspect_err(|_| forget_connection())?;
    Ok(())
}

/// Synchronous `Notify` call; returns the server-assigned notification id.
/// Only the queue worker calls this in the app (which is what makes the
/// LAST_ID replace-chains race-free). Pub for the DBus round-trip
/// integration test.
pub fn send_blocking(
    slot: Slot,
    icon: &str,
    summary: &str,
    body: &str,
    transient: bool,
) -> zbus::Result<u32> {
    send_blocking_with(slot, icon, summary, body, transient, &[])
}

/// The one connection the notifications go out on: a server sends an
/// action's `ActionInvoked` to the connection that sent the notification
/// (GNOME does), so its one listener shares it. A failed call closes it
/// (which ends its listener); the next send connects again.
static CONN: Mutex<Option<Link>> = Mutex::new(None);
/// Which connection is the current one: a listener whose connection was
/// replaced hands nothing on (a server that broadcasts `ActionInvoked`
/// would reach it too).
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Listener threads alive.
static LISTENERS: AtomicUsize = AtomicUsize::new(0);
/// Listeners whose match rule is in place: a click sent from now on is
/// heard.
static SUBSCRIBED: AtomicUsize = AtomicUsize::new(0);

struct Link {
    conn: zbus::blocking::Connection,
    generation: u64,
    listening: bool,
    /// The server parses bodies as markup (asked once per connection).
    markup: Option<bool>,
}

impl Link {
    /// Start its listener, once, when there is a handler.
    fn hear(&mut self) {
        if !self.listening && ON_ACTION.get().is_some() {
            self.listening = listen(self.conn.clone(), self.generation);
        }
    }
}

/// Tests: the action listeners running.
#[doc(hidden)]
pub fn action_listeners() -> usize {
    LISTENERS.load(Ordering::SeqCst)
}

/// Tests: the id the server gave `slot`'s last notification, once this
/// side has it (a click on it is heard from then on; 0 before).
#[doc(hidden)]
pub fn last_id(slot: Slot) -> u32 {
    LAST_ID[slot as usize].load(Ordering::Relaxed)
}

/// Tests: the action listeners that hear clicks now (their match rule is
/// on the bus); a click sent before that is lost, as on a real desktop.
#[doc(hidden)]
pub fn action_listeners_subscribed() -> usize {
    SUBSCRIBED.load(Ordering::SeqCst)
}

fn connection() -> zbus::Result<zbus::blocking::Connection> {
    let mut c = CONN.lock().unwrap_or_else(|e| e.into_inner());
    if c.is_none() {
        let conn = zbus::blocking::connection::Builder::session()?
            .method_timeout(METHOD_TIMEOUT)
            .build()?;
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        *c = Some(Link {
            conn,
            generation,
            listening: false,
            markup: None,
        });
    }
    let link = c.as_mut().expect("just made");
    link.hear();
    Ok(link.conn.clone())
}

/// Drop the connection after a failed call, and close it: its listener
/// holds a clone and would otherwise wait on it for good.
fn forget_connection() {
    let link = CONN.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(link) = link {
        GENERATION.fetch_add(1, Ordering::SeqCst);
        let _ = link.conn.close();
    }
}

/// Hear `ActionInvoked` on `conn` (until it closes) and hand the clicks on
/// our own notifications to the handler, while `conn` is the current one.
/// False when its thread could not start (the next send tries again).
fn listen(conn: zbus::blocking::Connection, generation: u64) -> bool {
    let run = move || {
        LISTENERS.fetch_add(1, Ordering::SeqCst);
        hear_clicks(&conn, generation);
        LISTENERS.fetch_sub(1, Ordering::SeqCst);
    };
    std::thread::Builder::new()
        .name("hushmic-notify-actions".into())
        .spawn(run)
        .is_ok()
}

fn hear_clicks(conn: &zbus::blocking::Connection, generation: u64) {
    let Ok(proxy) = zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    ) else {
        return;
    };
    let Ok(clicks) = proxy.receive_signal("ActionInvoked") else {
        return;
    };
    // Replaced while subscribing: the connection may be closed already,
    // and a stream zbus adds after its reader has stopped (the bus's
    // AddMatch answered, then the end of the socket) never ends. A close
    // comes after the replacement, so this sees it.
    if GENERATION.load(Ordering::SeqCst) != generation {
        return;
    }
    SUBSCRIBED.fetch_add(1, Ordering::SeqCst);
    hand_on(clicks, generation);
    SUBSCRIBED.fetch_sub(1, Ordering::SeqCst);
}

fn hand_on(clicks: zbus::blocking::proxy::SignalIterator<'_>, generation: u64) {
    for msg in clicks {
        let Ok((id, key)) = msg.body().deserialize::<(u32, String)>() else {
            continue;
        };
        if GENERATION.load(Ordering::SeqCst) != generation {
            return; // replaced: the current connection's listener hears it
        }
        let slot = SLOTS
            .iter()
            .find(|s| id != 0 && LAST_ID[**s as usize].load(Ordering::Relaxed) == id);
        if let (Some(slot), Some(f)) = (slot, ON_ACTION.get()) {
            f(*slot, key);
        }
    }
}

/// [`send_blocking`] with actions, (key, label) pairs.
pub fn send_blocking_with(
    slot: Slot,
    icon: &str,
    summary: &str,
    body: &str,
    transient: bool,
    actions: &[(&str, &str)],
) -> zbus::Result<u32> {
    let idx = slot as usize;
    let replaces = LAST_ID[idx].load(Ordering::Relaxed);
    let mut hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    // Lets the desktop associate the bubble with the app's .desktop file
    // (app name + icon in notification centers). In a Flatpak the exported
    // entry is named after the app ID, not "hushmic".
    hints.insert(
        "desktop-entry",
        crate::sandbox::flatpak_app_id().unwrap_or("hushmic").into(),
    );
    if transient {
        hints.insert("transient", true.into());
    }
    // The spec's flat list: key, label, key, label…
    let flat: Vec<&str> = actions.iter().flat_map(|(k, l)| [*k, *l]).collect();
    let conn = connection()?;
    // a body can carry a name (a folder, a microphone) with & or < in it
    let body = plain_body(body, body_markup(&conn));
    let reply = conn
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "HushMic", replaces, icon, summary, &*body, flat, hints,
                -1i32, // expire: server default
            ),
        )
        .inspect_err(|_| forget_connection())?;
    let id: u32 = reply.body().deserialize()?;
    LAST_ID[idx].store(id, Ordering::Relaxed);
    Ok(id)
}

/// Whether the server parses bodies as markup ("body-markup" in its
/// capabilities): asked once per connection, without holding it locked.
fn body_markup(conn: &zbus::blocking::Connection) -> bool {
    let known = CONN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|l| l.markup);
    if let Some(m) = known {
        return m;
    }
    let caps: Vec<String> = conn
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "GetCapabilities",
            &(),
        )
        .ok()
        .and_then(|r| r.body().deserialize().ok())
        .unwrap_or_default();
    let m = caps.iter().any(|c| c == "body-markup");
    if let Some(l) = CONN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        l.markup = Some(m);
    }
    m
}

/// `body` as plain text to a server that reads markup (& < > escaped);
/// as it is to one that does not (it would show "&amp;").
fn plain_body(body: &str, markup: bool) -> std::borrow::Cow<'_, str> {
    if !markup || !body.contains(['&', '<', '>']) {
        return body.into();
    }
    body.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .into()
}

/// After this many consecutive watchdog attempts where enable() succeeded
/// yet the node still never appeared, the respawn loop is declared stuck
/// and surfaced (≈30 s of downtime under the backoff schedule).
const SILENT_FAILURE_THRESHOLD: u32 = 3;

/// This many successful respawns within [`RESPAWN_WINDOW`] means the child
/// keeps crashing and coming back — each cycle is a mic dropout the user
/// hears, so the pattern deserves one notification even though every single
/// respawn "succeeded".
const RESPAWN_THRESHOLD: usize = 3;
const RESPAWN_WINDOW: Duration = Duration::from_secs(180);

/// A recovery notice is only truthful once the node has stayed up for a
/// while: right after a respawn the next crash of a flapping child may be
/// seconds away.
const STABLE_AFTER: Duration = Duration::from_secs(30);

/// Decides *when* a failure deserves a notification, so the watchdog's
/// backoff-gated retries do not re-pop the same bubble forever:
/// user-initiated attempts always surface, retries only surface a *changed*
/// message, a respawn loop that keeps "succeeding" without producing the
/// node surfaces once at a threshold, rapid crash/respawn cycles surface as
/// instability, and a recovery notice is emitted exactly when a previously
/// surfaced failure clears AND the node has been stable for a while. Pure
/// state machine — the caller does the actual sending.
pub struct FailureGate {
    /// Dedup key of the last failure recorded (enable error message, or an
    /// internal marker).
    last: Option<String>,
    silent_fails: u32,
    /// Whether any failure was actually surfaced since the last reset —
    /// only then is a recovery notice warranted.
    notified: bool,
    /// Timestamps of recent successful watchdog respawns (time-decayed by
    /// [`RESPAWN_WINDOW`]); deliberately survives `on_healthy` so a flap
    /// pattern spanning healthy ticks is still visible.
    respawns: VecDeque<Instant>,
    /// A failure (or a flap) since the chain was last up: the next time it
    /// is up and not flapping, it is running again.
    down: bool,
    /// How many times the chain ran again after a failure
    /// ([`FailureGate::on_up`]); survives a reset, so it only ever grows.
    recoveries: u64,
}

/// Internal dedup keys. They start with NUL so no real enable() error
/// message can collide with them.
const SILENT_KEY: &str = "\0silent-failure";
const FLAP_KEY: &str = "\0flapping";

impl FailureGate {
    pub fn new() -> Self {
        FailureGate {
            last: None,
            silent_fails: 0,
            notified: false,
            respawns: VecDeque::new(),
            down: false,
            recoveries: 0,
        }
    }

    /// enable() failed with `msg`. True => surface it now. Direct user
    /// actions (tray clicks, launch) always get feedback; watchdog retries
    /// only when the message changed since the last recorded failure.
    pub fn on_enable_error(&mut self, msg: &str, user_initiated: bool) -> bool {
        self.down = true;
        self.silent_fails = 0;
        let show = user_initiated || self.last.as_deref() != Some(msg);
        self.last = Some(msg.to_string());
        self.notified |= show;
        show
    }

    /// A watchdog attempt where enable() returned Ok but `hushmic_source`
    /// never appeared. True once, at the stuck-loop threshold.
    pub fn on_silent_failure(&mut self) -> bool {
        self.down = true;
        self.silent_fails = self.silent_fails.saturating_add(1);
        if self.silent_fails < SILENT_FAILURE_THRESHOLD {
            return false;
        }
        let show = self.last.as_deref() != Some(SILENT_KEY);
        self.last = Some(SILENT_KEY.to_string());
        self.notified |= show;
        show
    }

    /// A watchdog respawn attempt SUCCEEDED (node back up). True once when
    /// the recent-respawn rate crosses the flap threshold: individually
    /// successful respawns in quick succession mean the child keeps dying.
    pub fn on_respawn(&mut self) -> bool {
        self.on_respawn_at(Instant::now())
    }

    fn on_respawn_at(&mut self, now: Instant) -> bool {
        self.respawns.push_back(now);
        while let Some(t) = self.respawns.front() {
            if now.duration_since(*t) > RESPAWN_WINDOW {
                self.respawns.pop_front();
            } else {
                break;
            }
        }
        if self.respawns.len() < RESPAWN_THRESHOLD {
            return false;
        }
        let show = self.last.as_deref() != Some(FLAP_KEY);
        self.last = Some(FLAP_KEY.to_string());
        self.notified |= show;
        self.down = true;
        show
    }

    /// The node is up (every healthy look; after [`FailureGate::on_healthy`]
    /// in the same look). True the first time after a failure, or after a
    /// flap once it is stable: the chain runs again. This is the window's
    /// "running again" (at once; the desktop notification waits for
    /// [`FailureGate::on_healthy`]).
    pub fn on_up(&mut self) -> bool {
        if !self.down || self.flapping() {
            return false;
        }
        self.down = false;
        self.recoveries += 1;
        true
    }

    /// The window showed the chain reconnecting (its node went, the
    /// watchdog brought it back, PipeWire restarted) and it is ready again:
    /// running again, without a notification for a routine reconnect. True
    /// when that counts (never mid-flap).
    pub fn on_reconnected(&mut self) -> bool {
        if self.flapping() {
            return false;
        }
        self.down = false;
        self.recoveries += 1;
        true
    }

    /// The node is confirmed up. True exactly when a failure was surfaced
    /// earlier AND the node has been stable (no respawn for a while) — the
    /// user saw an error bubble, so close the loop with a recovery notice,
    /// but never claim "running again" in the middle of a flap cycle.
    pub fn on_healthy(&mut self) -> bool {
        self.on_healthy_at(Instant::now())
    }

    fn on_healthy_at(&mut self, now: Instant) -> bool {
        let stable = self
            .respawns
            .back()
            .is_none_or(|t| now.duration_since(*t) >= STABLE_AFTER);
        if !stable {
            return false;
        }
        let recovered = self.notified;
        // The respawn history deliberately survives (time-decayed): it is
        // what distinguishes "stable again" from "between two crashes".
        self.last = None;
        self.silent_fails = 0;
        self.notified = false;
        recovered
    }

    /// The chain keeps restarting: from the flap threshold
    /// ([`FailureGate::on_respawn`]) until it is stable again, or fails
    /// outright instead.
    pub fn flapping(&self) -> bool {
        self.last.as_deref() == Some(FLAP_KEY)
    }

    /// How many times the chain ran again after a failure
    /// ([`FailureGate::on_up`]); it only grows, so a change means a new
    /// one.
    pub fn recoveries(&self) -> u64 {
        self.recoveries
    }

    /// Silent full reset (the user turned suppression off): stale failure
    /// state must not produce a "running again" notice later.
    pub fn reset(&mut self) {
        *self = FailureGate {
            recoveries: self.recoveries,
            ..FailureGate::new()
        };
    }
}

impl Default for FailureGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod direct_after_tests {
    use crate::appstate::mock::{state, Scenario};
    use crate::appstate::{AppRef, Mode};

    /// UX panel A3: after Off or Quit, the apps that used HushMic by name,
    /// and the microphone they hear now.
    #[test]
    fn off_or_quit_says_who_hears_the_microphone_directly() {
        crate::i18n::pin_english();
        let mut s = state(Scenario::OnInCall);
        let app = |n: Option<&str>| AppRef {
            name: n.map(str::to_string),
            ..AppRef::default()
        };
        let mic = s
            .mic
            .system_default
            .as_deref()
            .and_then(|m| s.description_of(m))
            .unwrap()
            .to_string();
        s.apps.using = vec![app(Some("Zoom"))];
        assert_eq!(
            super::direct_after(&s).unwrap(),
            format!("Zoom now hears {mic} directly, without noise suppression.")
        );
        s.apps.using = vec![app(Some("Zoom")), app(None)];
        assert_eq!(
            super::direct_after(&s).unwrap(),
            format!("Zoom and An app now hear {mic} directly, without noise suppression.")
        );
        s.apps.using = vec![app(Some("Zoom")), app(Some("OBS")), app(Some("Discord"))];
        assert_eq!(
            super::direct_after(&s).unwrap(),
            format!("3 apps now hear {mic} directly, without noise suppression.")
        );
        s.mic.system_default = None;
        assert!(super::direct_after(&s)
            .unwrap()
            .contains("hear your microphone directly"));
        // nobody to tell, or nothing turned off
        s.mode = Mode::Off;
        assert_eq!(super::direct_after(&s), None);
        s.mode = Mode::Suppress;
        s.apps.using.clear();
        assert_eq!(super::direct_after(&s), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_switch_notifications_name_both_microphones() {
        crate::i18n::pin_english();
        assert_eq!(
            crate::tr!(
                "notify-mic-fallback-body",
                mic = "RODE NT-USB",
                standin = "Built-in Audio"
            ),
            "RODE NT-USB was disconnected. HushMic uses Built-in Audio for now."
        );
        assert_eq!(
            crate::tr!(
                "notify-mic-return-body",
                mic = "RODE NT-USB",
                standin = "Built-in Audio"
            ),
            "RODE NT-USB is back. HushMic switched back to it from Built-in Audio."
        );
        assert_eq!(
            crate::tr!(
                "notify-default-switched-body",
                new = "Jabra",
                old = "Built-in Audio"
            ),
            "HushMic now cleans Jabra, the new default microphone (it was Built-in Audio)."
        );
    }

    use crate::appstate::Page;

    fn note(summary: &str) -> Note {
        Note {
            slot: Slot::Status,
            icon: "dialog-error".into(),
            summary: summary.into(),
            body: String::new(),
            transient: false,
            actions: Vec::new(),
        }
    }

    fn summaries(notes: &[Note]) -> Vec<&str> {
        notes.iter().map(|n| n.summary.as_str()).collect()
    }

    #[test]
    fn held_problems_go_out_once_if_still_so_when_no_longer_shown() {
        let mut h = Problems::default();
        assert!(h.arrive(Problem::Failure, note("failed"), true).is_none());
        assert!(h
            .arrive(Problem::MicFallback, note("unplugged"), true)
            .is_none());
        assert!(h
            .arrive(Problem::Failure, note("failed again"), true)
            .is_none());
        assert!(h
            .arrive(Problem::DefaultReleased, note("released"), true)
            .is_none());
        // the failure and the fallback are still so; the default came back
        let (send, dropped) = h.release(
            0,
            |p| matches!(p, Problem::Failure | Problem::MicFallback),
            true,
        );
        // the newer failure, in the first failure's place
        assert_eq!(summaries(&send), ["failed again", "unplugged"]);
        assert_eq!(dropped, [Problem::DefaultReleased]);
        // once: nothing stays held
        assert!(h.is_empty());
        let (send, dropped) = h.release(0, |_| true, true);
        assert!(send.is_empty() && dropped.is_empty());
    }

    #[test]
    fn a_moment_is_never_sent_late() {
        let mut h = Problems::default();
        assert!(h.arrive(Problem::MicBack, note("back"), true).is_none());
        let (send, dropped) = h.release(0, |_| true, true);
        assert!(send.is_empty());
        assert_eq!(dropped, [Problem::MicBack]);
    }

    #[test]
    fn running_again_answers_a_failure_bubble_and_nothing_else() {
        // a bubble went out while the window was elsewhere; the user
        // looks at the window as it recovers: the bubble is replaced
        let mut h = Problems::default();
        assert!(h.arrive(Problem::Failure, note("failed"), false).is_some());
        assert!(h.arrive(Problem::Recovered, note("again"), true).is_some());
        // and only once
        assert!(h.arrive(Problem::Recovered, note("again"), false).is_none());

        // held and over before the window stopped showing it: no bubble,
        // so no "running again" either, shown or not
        let mut h = Problems::default();
        assert!(h.arrive(Problem::Failure, note("failed"), true).is_none());
        assert!(h.arrive(Problem::Recovered, note("again"), true).is_none());
        assert!(h.arrive(Problem::Recovered, note("again"), false).is_none());
        let (send, dropped) = h.release(0, |_| false, true);
        assert!(send.is_empty());
        assert_eq!(dropped, [Problem::Failure]);

        // a release sends the failure while a flap is over: the bubble is
        // up, so the recovery answers it
        let mut h = Problems::default();
        h.arrive(Problem::Failure, note("failed"), true);
        h.arrive(Problem::Flapping, note("flapping"), true);
        let (send, dropped) = h.release(0, |p| p == Problem::Failure, true);
        assert_eq!(summaries(&send), ["failed"]);
        assert_eq!(dropped, [Problem::Flapping]);
        assert!(h.arrive(Problem::Recovered, note("again"), true).is_some());

        // turned off since: a later recovery does not answer that bubble
        let mut h = Problems::default();
        h.arrive(Problem::Flapping, note("flapping"), false);
        h.forget_failure();
        assert!(h.arrive(Problem::Recovered, note("again"), false).is_none());

        // notifications off at the release: nothing sent, no bubble up
        let mut h = Problems::default();
        h.arrive(Problem::Failure, note("failed"), true);
        let (send, dropped) = h.release(0, |_| true, false);
        assert!(send.is_empty());
        assert_eq!(dropped, [Problem::Failure]);
        assert!(h.arrive(Problem::Recovered, note("again"), false).is_none());
    }

    #[test]
    fn a_release_sends_lasting_notes_last_and_failures_on_their_own_chain() {
        let mut h = Problems::default();
        h.arrive(Problem::Rerouted, note("rerouted"), true);
        h.arrive(
            Problem::MicFallback,
            Note {
                transient: true,
                actions: Vec::new(),
                ..note("unplugged")
            },
            true,
        );
        let (send, _) = h.release(0, |_| true, true);
        assert_eq!(summaries(&send), ["unplugged", "rerouted"]);
        for p in Problem::ALL {
            let chain = matches!(p, Problem::Failure | Problem::Flapping | Problem::Recovered);
            assert_eq!(p.slot() == Slot::Chain, chain, "{p:?}");
        }
        // one chain per topic (UX panel round 2, A5): a microphone that
        // leaves never replaces "No sound…", nor the default's notice
        let topic = |p: Problem| p.slot();
        assert_eq!(topic(Problem::MicFallback), topic(Problem::MicBack));
        assert_eq!(topic(Problem::Rerouted), topic(Problem::MicStuck));
        let apart = [
            Problem::Failure,
            Problem::MicFallback,
            Problem::MicSilent,
            Problem::DefaultReleased,
            Problem::Rerouted,
        ];
        for (i, a) in apart.iter().enumerate() {
            for b in &apart[i + 1..] {
                assert_ne!(topic(*a), topic(*b), "{a:?} and {b:?}");
            }
        }
        for slot in SLOTS {
            assert_eq!(SLOTS[slot as usize], slot, "LAST_ID order");
        }
    }

    #[test]
    fn what_the_window_still_shows_stays_held() {
        let mut h = Problems::default();
        h.arrive(Problem::Failure, note("failed"), true);
        h.arrive(Problem::MicFallback, note("unplugged"), true);
        // Live (focused) to first run: the failure is still shown there
        let (send, dropped) =
            h.release(shown_on(Page::FirstRun, LiveShows::Untold), |_| true, true);
        assert_eq!(summaries(&send), ["unplugged"]);
        assert!(dropped.is_empty());
        assert!(!h.is_empty());
        // then Files: nothing shown there, the failure goes
        let (send, _) = h.release(shown_on(Page::Files, LiveShows::Untold), |_| true, true);
        assert_eq!(send.len(), 1);
        assert!(h.is_empty());
    }

    /// Live's Status card shows one problem at a time: only that one is
    /// held back; one it can't show (a lower one) goes out as a
    /// notification. Its moments stay claimed.
    #[test]
    fn live_claims_only_the_problem_its_status_card_shows() {
        let live = |l| shown_on(Page::Live, l);
        let rerouted = live(LiveShows::One(Problem::Rerouted));
        assert_ne!(rerouted & Problem::Rerouted.bit(), 0);
        assert_eq!(rerouted & Problem::MicFallback.bit(), 0);
        assert_eq!(rerouted & Problem::Failure.bit(), 0);
        for moment in [Problem::Recovered, Problem::MicBack] {
            assert_ne!(rerouted & moment.bit(), 0, "{moment:?}");
            assert_ne!(live(LiveShows::Nothing) & moment.bit(), 0, "{moment:?}");
        }
        assert_eq!(
            live(LiveShows::Nothing),
            Problem::Recovered.bit() | Problem::MicBack.bit()
        );
        // a fallback under a rerouted capture: not held for Live's card,
        // it reaches the desktop (once its grace is over:
        // `a_problem_live_may_show_waits_for_the_cards_word`)
        let mut h = Problems::default();
        let shown = |p: Problem| rerouted & p.bit() != 0;
        let sent = h.arrive(
            Problem::MicFallback,
            note("unplugged"),
            shown(Problem::MicFallback),
        );
        assert_eq!(sent.map(|n| n.summary), Some("unplugged".to_string()));
        // held while the card showed it, it goes once the card moves on
        assert!(h
            .arrive(Problem::Rerouted, note("taken"), shown(Problem::Rerouted))
            .is_none());
        let (send, _) = h.release(live(LiveShows::Nothing), |_| true, true);
        assert_eq!(summaries(&send), ["taken"]);
        // the words the window sends
        for p in [
            Problem::Failure,
            Problem::Flapping,
            Problem::Rerouted,
            Problem::MicFallback,
        ] {
            assert_eq!(Problem::parse(p.word().unwrap()), Some(p));
        }
        assert_eq!(Problem::DefaultReleased.word(), None);
        assert_eq!(Problem::parse("default-released"), None);
    }

    /// The daemon decides when a problem arrives, before Live's Status
    /// card has seen the state that carries it: one the card could show
    /// waits [`CARD_GRACE`] for the card's word, and only then goes to the
    /// desktop if the card shows something else.
    #[test]
    fn a_problem_live_may_show_waits_for_the_cards_word() {
        let live = |l| shown_on(Page::Live, l);
        let t0 = Instant::now();
        let grace = Some(t0 + CARD_GRACE);
        let after = t0 + CARD_GRACE + Duration::from_millis(1);

        // healthy Live (told "none"): the fallback is held, not sent
        let mut h = Problems::default();
        let nothing = live(LiveShows::Nothing);
        let shown = nothing & Problem::MicFallback.bit() != 0;
        assert!(!shown);
        assert!(h
            .arrive_until(Problem::MicFallback, note("unplugged"), shown, grace)
            .is_none());
        let (send, dropped) = h.release_at(nothing, Some(t0), |_| true, true);
        assert!(send.is_empty() && dropped.is_empty());
        // the card then says it shows the fallback: held past the grace
        let fallback = live(LiveShows::One(Problem::MicFallback));
        let (send, dropped) = h.release_at(fallback, Some(after), |_| true, true);
        assert!(send.is_empty() && dropped.is_empty());
        // and goes once the card moves on while it still holds
        let (send, _) = h.release_at(nothing, Some(after), |_| true, true);
        assert_eq!(summaries(&send), ["unplugged"]);

        // the card shows a rerouted capture: the fallback waits out the
        // grace, then reaches the desktop
        let mut h = Problems::default();
        let rerouted = live(LiveShows::One(Problem::Rerouted));
        assert!(h
            .arrive_until(Problem::MicFallback, note("unplugged"), false, grace)
            .is_none());
        let (send, _) = h.release_at(rerouted, Some(t0), |_| true, true);
        assert!(send.is_empty());
        let (send, _) = h.release_at(rerouted, Some(after), |_| true, true);
        assert_eq!(summaries(&send), ["unplugged"]);

        // the window leaves Live within the grace: no longer waiting
        let mut h = Problems::default();
        assert!(h
            .arrive_until(Problem::MicFallback, note("unplugged"), false, grace)
            .is_none());
        let (send, _) = h.release_at(0, None, |_| true, true);
        assert_eq!(summaries(&send), ["unplugged"]);

        // over before the grace ends: nothing to send
        let mut h = Problems::default();
        assert!(h
            .arrive_until(Problem::MicFallback, note("unplugged"), false, grace)
            .is_none());
        let (send, dropped) = h.release_at(rerouted, Some(after), |_| false, true);
        assert!(send.is_empty());
        assert_eq!(dropped, [Problem::MicFallback]);

        // with no grace (not on Live, or not focused) it goes at once
        let mut h = Problems::default();
        let sent = h.arrive_until(Problem::MicFallback, note("unplugged"), false, None);
        assert_eq!(sent.map(|n| n.summary), Some("unplugged".to_string()));
    }

    #[test]
    fn each_page_shows_its_problems() {
        let shows = |page, p: Problem| shown_on(page, LiveShows::Untold) & p.bit() != 0;
        for p in Problem::ALL {
            // the default released is Settings › Apps' note, never Live's
            assert_eq!(
                shows(Page::Live, p),
                p != Problem::DefaultReleased,
                "Live {p:?}"
            );
            assert_eq!(
                shows(Page::Settings, p),
                p == Problem::DefaultReleased,
                "Settings {p:?}"
            );
            for page in [Page::Files, Page::Shortcuts, Page::About] {
                assert!(!shows(page, p), "{page:?} shows no {p:?}");
            }
            assert_eq!(
                shows(Page::FirstRun, p),
                p == Problem::Failure,
                "FirstRun {p:?}"
            );
        }
        // one bit each
        let all = Problem::ALL.iter().fold(0u16, |m, p| {
            assert_eq!(m & p.bit(), 0);
            m | p.bit()
        });
        assert_eq!(all.count_ones(), Problem::ALL.len() as u32);
    }

    #[test]
    fn the_first_close_notice_waits_and_a_shutdown_or_logout_cancels_it() {
        // a plain close: sent once the watch says the session stays, once
        let mut f = FirstClose::default();
        assert!(f.closed(false, true));
        assert!(f.due(true, false, true));
        assert!(!f.due(true, false, true));
        // the daemon shuts down meanwhile: nothing
        let mut f = FirstClose::default();
        assert!(f.closed(false, true));
        f.shutdown();
        assert!(!f.due(true, false, true));
        // the session went away (a logout): nothing, and a later close
        // still may tell
        let mut f = FirstClose::default();
        assert!(f.closed(false, true));
        assert!(!f.due(false, false, true));
        assert!(f.closed(false, true));
        assert!(f.due(true, false, true));
        // sent before, or notifications off: never armed
        let mut f = FirstClose::default();
        assert!(!f.closed(true, true));
        assert!(!f.closed(false, false));
        assert!(!f.due(true, false, true));
        // the window open again by then: nothing, and a later close may
        let mut f = FirstClose::default();
        assert!(f.closed(false, true));
        assert!(!f.due(true, true, true));
        assert!(f.closed(false, true));
        // a second close while one is watched does not arm again
        let mut f = FirstClose::default();
        assert!(f.closed(false, true));
        assert!(!f.closed(false, true));
    }

    #[test]
    fn a_session_that_closes_or_goes_within_the_watch_is_a_logout() {
        let run = |looks: &[SessionLook]| -> (bool, usize) {
            let mut seen = 0;
            let mut waits = 0;
            let stays = watch_session(
                || {
                    seen += 1;
                    looks
                        .get(seen - 1)
                        .cloned()
                        .unwrap_or_else(|| looks.last().cloned().flatten())
                },
                |_| waits += 1,
            );
            (stays, waits)
        };
        let active = Some(("active".to_string(), true));
        let full = (CLOSE_NOTICE_WATCH.as_millis() / WATCH_STEP.as_millis()) as usize;
        // active throughout: stays, after the whole watch
        assert_eq!(run(std::slice::from_ref(&active)), (true, full));
        // closing after a second: a logout, told at once
        let closing = Some(("closing".to_string(), true));
        assert_eq!(run(&[active.clone(), active.clone(), closing]), (false, 2));
        // gone
        assert_eq!(run(&[active.clone(), None]), (false, 1));
        // still there but no longer active at the end (another user's
        // session in front): not now
        let inactive = Some(("online".to_string(), false));
        assert_eq!(run(&[inactive]), (false, full));
    }

    #[test]
    fn flapping_lasts_until_stable() {
        let mut g = FailureGate::new();
        let t = Instant::now();
        for i in 0..RESPAWN_THRESHOLD as u64 {
            assert!(!g.flapping());
            g.on_respawn_at(t + Duration::from_secs(i));
        }
        assert!(g.flapping());
        // healthy between two crashes: still flapping, no recovery
        assert!(!g.on_healthy_at(t + Duration::from_secs(5)));
        assert!(g.flapping());
        // stable: over
        assert!(g.on_healthy_at(t + Duration::from_secs(3) + STABLE_AFTER));
        assert!(!g.flapping());
        // a failure outright is not flapping
        for i in 0..RESPAWN_THRESHOLD as u64 {
            g.on_respawn_at(t + Duration::from_secs(100 + i));
        }
        assert!(g.flapping());
        g.on_enable_error("gone", false);
        assert!(!g.flapping());
    }

    #[test]
    fn the_window_hears_running_again_at_once_never_mid_flap() {
        let mut g = FailureGate::new();
        let t = Instant::now();
        // up with nothing wrong before: nothing to say
        assert!(!g.on_up());
        // a failure, then up: at once (the bubble waits for stability)
        g.on_enable_error("no model", false);
        assert!(g.on_up());
        assert_eq!(g.recoveries(), 1);
        assert!(!g.on_up(), "once");
        // a failure the user retried (user-initiated): the same
        g.on_enable_error("no model", true);
        assert!(g.on_up());
        // a stuck chain, then up
        g.on_silent_failure();
        assert!(g.on_up());
        assert_eq!(g.recoveries(), 3);
        // a flap: not while it lasts, then once it is stable
        for i in 0..RESPAWN_THRESHOLD as u64 {
            g.on_respawn_at(t + Duration::from_secs(i));
        }
        assert!(!g.on_up(), "mid-flap");
        assert!(!g.on_healthy_at(t + Duration::from_secs(5)));
        assert!(!g.on_up(), "between two crashes");
        assert!(g.on_healthy_at(t + Duration::from_secs(3) + STABLE_AFTER));
        assert!(g.on_up(), "stable again");
        assert_eq!(g.recoveries(), 4);
        // a reconnect the window showed: counts too, never mid-flap, and
        // it answers a failure noted before it (no second count at up)
        g.on_enable_error("gone", false);
        assert!(g.on_reconnected());
        assert_eq!(g.recoveries(), 5);
        assert!(!g.on_up());
        for i in 0..RESPAWN_THRESHOLD as u64 {
            g.on_respawn_at(t + Duration::from_secs(200 + i));
        }
        assert!(!g.on_reconnected(), "mid-flap");
        assert_eq!(g.recoveries(), 5);
        assert!(g.on_healthy_at(t + Duration::from_secs(203) + STABLE_AFTER));
        assert!(g.on_up());
        assert_eq!(g.recoveries(), 6);
        // turned off by the user: no "running again" for what came before,
        // and the count never goes back
        g.on_enable_error("gone", false);
        g.reset();
        assert!(!g.on_up());
        assert_eq!(g.recoveries(), 6);
    }

    #[test]
    fn gate_defaults_on_and_flips() {
        assert!(enabled());
        set_enabled(false);
        assert!(!enabled());
        set_enabled(true);
        assert!(enabled());
    }

    #[test]
    fn watchdog_retries_with_same_error_surface_once() {
        let mut g = FailureGate::new();
        assert!(g.on_enable_error("model missing", false));
        assert!(!g.on_enable_error("model missing", false));
        assert!(!g.on_enable_error("model missing", false));
        // a different failure is news again
        assert!(g.on_enable_error("plugin missing", false));
        assert!(!g.on_enable_error("plugin missing", false));
    }

    #[test]
    fn user_initiated_attempts_always_get_feedback() {
        let mut g = FailureGate::new();
        assert!(g.on_enable_error("model missing", true));
        assert!(g.on_enable_error("model missing", true));
        // and they update the dedup key for subsequent watchdog retries
        assert!(!g.on_enable_error("model missing", false));
    }

    #[test]
    fn healthy_resets_dedup_so_a_relapse_surfaces_again() {
        let mut g = FailureGate::new();
        assert!(g.on_enable_error("model missing", false));
        assert!(g.on_healthy());
        assert!(g.on_enable_error("model missing", false));
    }

    #[test]
    fn silent_failures_surface_once_at_threshold() {
        let mut g = FailureGate::new();
        assert!(!g.on_silent_failure());
        assert!(!g.on_silent_failure());
        assert!(g.on_silent_failure()); // threshold = 3
        assert!(!g.on_silent_failure()); // no re-pop while stuck
        assert!(g.on_healthy());
        // relapse after recovery: counts from zero again
        assert!(!g.on_silent_failure());
        assert!(!g.on_silent_failure());
        assert!(g.on_silent_failure());
    }

    #[test]
    fn enable_error_resets_the_silent_streak() {
        let mut g = FailureGate::new();
        assert!(!g.on_silent_failure());
        assert!(!g.on_silent_failure());
        assert!(g.on_enable_error("model missing", false));
        // the streak restarts: two more silent failures are below threshold
        assert!(!g.on_silent_failure());
        assert!(!g.on_silent_failure());
        assert!(g.on_silent_failure());
    }

    #[test]
    fn recovery_notice_only_after_a_surfaced_failure() {
        let mut g = FailureGate::new();
        assert!(!g.on_healthy()); // nothing ever surfaced -> no notice
        assert!(!g.on_silent_failure());
        assert!(!g.on_healthy()); // failure was never SURFACED -> no notice
        assert!(g.on_enable_error("x", false));
        assert!(g.on_healthy());
        assert!(!g.on_healthy()); // one notice per outage, not per tick
    }

    #[test]
    fn reset_is_silent() {
        let mut g = FailureGate::new();
        assert!(g.on_enable_error("x", false));
        g.reset(); // user turned the feature off
        assert!(!g.on_healthy()); // no "running again" for a manual off
    }

    #[test]
    fn rapid_respawns_surface_as_flapping_exactly_once() {
        let mut g = FailureGate::new();
        let t0 = Instant::now();
        assert!(!g.on_respawn_at(t0));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(30)));
        assert!(g.on_respawn_at(t0 + Duration::from_secs(60))); // 3 within the window
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(90))); // no re-pop while flapping
    }

    #[test]
    fn slow_respawns_are_not_flapping() {
        let mut g = FailureGate::new();
        let t0 = Instant::now();
        // one respawn every 100 s: only 2 ever fall inside the 180 s window
        assert!(!g.on_respawn_at(t0));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(100)));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(200)));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(300)));
    }

    #[test]
    fn no_recovery_notice_in_the_middle_of_a_flap_cycle() {
        let mut g = FailureGate::new();
        let t0 = Instant::now();
        assert!(!g.on_respawn_at(t0));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(30)));
        assert!(g.on_respawn_at(t0 + Duration::from_secs(60))); // flap surfaced
                                                                // healthy ticks right after a respawn: NOT stable yet -> hold
        assert!(!g.on_healthy_at(t0 + Duration::from_secs(65)));
        assert!(!g.on_healthy_at(t0 + Duration::from_secs(80)));
        // 30 s without a respawn: stable -> recovery notice fires once
        assert!(g.on_healthy_at(t0 + Duration::from_secs(90)));
        assert!(!g.on_healthy_at(t0 + Duration::from_secs(95)));
    }

    #[test]
    fn flapping_can_surface_again_after_a_stable_recovery() {
        let mut g = FailureGate::new();
        let t0 = Instant::now();
        for i in 0..3 {
            g.on_respawn_at(t0 + Duration::from_secs(i * 10));
        }
        assert!(g.on_healthy_at(t0 + Duration::from_secs(300))); // stable recovery
                                                                 // a fresh burst of respawns is news again
        let t1 = t0 + Duration::from_secs(1000);
        assert!(!g.on_respawn_at(t1));
        assert!(!g.on_respawn_at(t1 + Duration::from_secs(10)));
        assert!(g.on_respawn_at(t1 + Duration::from_secs(20)));
    }

    #[test]
    fn a_body_is_plain_text_whether_or_not_the_server_reads_markup() {
        let b = "Saved in you/A & <B>.";
        assert_eq!(plain_body(b, true), "Saved in you/A &amp; &lt;B&gt;.");
        assert_eq!(plain_body(b, false), b);
        assert_eq!(
            plain_body("Saved in Recordings/Cleaned.", true),
            "Saved in Recordings/Cleaned."
        );
    }
}
