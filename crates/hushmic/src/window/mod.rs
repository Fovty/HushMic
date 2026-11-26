//! The v1 companion window: one process, a child of the daemon (like the
//! old Mic check window), with the pages routed by `--page`. The daemon
//! keeps one at a time, re-creates it on a relaunch (Wayland cannot raise
//! another process's window) and remembers its page; the window dies with
//! the daemon.
//!
//! The shell owns the process and the window (size, app id, the first
//! frame held for the daemon's first snapshot); everything inside it is the
//! page layer's ([`Layer`]: uikit and the pages). [`link`] is how a layer
//! reads the state and asks for changes.

pub mod diagnostics;
pub mod dnd;
pub mod gate;
pub mod link;
pub mod parent;
mod wmclass;

pub use link::Link;

use crate::appstate::{mock::Scenario, Action, Page, Theme};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// How long the first frame waits for the daemon's first snapshot, so the
/// window never opens on a wrong state (eframe keeps it hidden meanwhile).
pub const FIRST_FRAME_HOLD: Duration = Duration::from_millis(150);
/// The boards' window: default and minimum content size.
pub const DEFAULT_SIZE: [f32; 2] = [960.0, 640.0];
pub const MIN_SIZE: [f32; 2] = [720.0, 520.0];
/// Room left on the monitor around the window for panels and the title
/// bar: a size, minimum or not, never takes more than the monitor less
/// this (UX panel A9: at 1.5x text the minimum outgrew a 1280x800
/// screen and More hid behind the panel).
pub const SCREEN_ROOM: [f32; 2] = [32.0, 112.0];
/// The least a fitted size goes down to, however small the monitor.
const FIT_FLOOR: [f32; 2] = [320.0, 240.0];

/// `size` (points), no larger than the monitor (points; None: unknown)
/// less [`SCREEN_ROOM`].
pub fn fit_to(monitor: Option<egui::Vec2>, size: [f32; 2]) -> egui::Vec2 {
    let size = egui::vec2(size[0], size[1]);
    match monitor {
        Some(m) if m.x > 0.0 && m.y > 0.0 => {
            let room = (m - egui::vec2(SCREEN_ROOM[0], SCREEN_ROOM[1]))
                .max(egui::vec2(FIT_FLOOR[0], FIT_FLOOR[1]));
            size.min(room)
        }
        _ => size,
    }
}

/// `hushmic --window [--page PAGE] [--mock SCENARIO] [--size WxH] [--theme dark|light]
/// [--reduced-motion]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    /// The page to open on; None = the one the daemon remembers.
    pub page: Option<Page>,
    /// Run on a board state instead of the daemon (building, screenshots).
    pub mock: Option<Scenario>,
    /// The size to open at (logical points); None = the default.
    pub size: Option<[u16; 2]>,
    /// The look to paint the first frame in, before the first snapshot
    /// (the daemon passes the snapshot's `desktop.effective_theme` and
    /// `reduced_motion`); None = not given (dark). The snapshots rule
    /// after that.
    pub theme: Option<Theme>,
    pub reduced_motion: bool,
}

pub fn usage() -> String {
    let pages: Vec<&str> = Page::ALL.iter().map(|p| p.word()).collect();
    format!(
        "usage: hushmic --window [--page {}] [--mock SCENARIO] [--size WxH] \
         [--theme dark|light] [--reduced-motion]",
        pages.join("|")
    )
}

/// Parse the words after `--window`.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--page" => {
                let w = it.next().ok_or_else(usage)?;
                opts.page =
                    Some(Page::parse(w).ok_or_else(|| format!("unknown page '{w}'\n{}", usage()))?);
            }
            "--mock" => {
                let w = it.next().ok_or_else(usage)?;
                opts.mock = Some(Scenario::parse(w).ok_or_else(|| {
                    let all: Vec<&str> = Scenario::ALL.iter().map(|s| s.word()).collect();
                    format!("unknown scenario '{w}' (scenarios: {})", all.join(" "))
                })?);
            }
            "--size" => {
                let w = it.next().ok_or_else(usage)?;
                let size = w
                    .split_once('x')
                    .and_then(|(a, b)| Some([a.parse().ok()?, b.parse().ok()?]))
                    .ok_or_else(|| format!("bad size '{w}' (WIDTHxHEIGHT)"))?;
                opts.size = Some(size);
            }
            "--theme" => {
                opts.theme = Some(match it.next().map(String::as_str) {
                    Some("dark") => Theme::Dark,
                    Some("light") => Theme::Light,
                    Some(w) => {
                        return Err(format!("unknown theme '{w}' (dark|light)\n{}", usage()))
                    }
                    None => return Err(usage()),
                });
            }
            "--reduced-motion" => opts.reduced_motion = true,
            other => return Err(format!("unknown option '{other}'\n{}", usage())),
        }
    }
    Ok(opts)
}

/// Routing between the shell and the layer: the page to draw. A layer
/// moves by setting `page`; the shell tells the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nav {
    pub page: Page,
}

/// The page layer: draws everything inside the window, every frame.
pub trait Layer {
    fn ui(&mut self, ctx: &egui::Context, link: &Link, nav: &mut Nav);
}

/// Builds the layer once the window exists (fonts and textures go in
/// here).
pub type MakeLayer = fn(&eframe::CreationContext<'_>, &Options) -> Box<dyn Layer>;

/// A resize is reported once the size has held this long (not on every
/// frame of a drag).
const SIZE_SETTLE: Duration = Duration::from_secs(1);

/// The default size is asked again (`Shell::reassert_size`) at most this
/// many times, this far apart, this long after the first frame.
const REASSERTS: u8 = 3;
const REASSERT_GAP: Duration = Duration::from_millis(500);
const REASSERT_FOR: Duration = Duration::from_secs(3);

/// The default size asked again on an open without a size of our own.
struct Reassert {
    left: u8,
    first_frame: Option<Instant>,
    last: Option<Instant>,
}

/// The monitor's size in points, once the window knows it.
fn monitor(ctx: &egui::Context) -> Option<egui::Vec2> {
    ctx.input(|i| i.viewport().monitor_size)
        .filter(|m| m.x > 0.0 && m.y > 0.0)
}

/// The size to open at: the remembered one, never under the minimum.
pub fn open_size(remembered: Option<[u16; 2]>) -> [f32; 2] {
    match remembered {
        Some([w, h]) => [f32::from(w).max(MIN_SIZE[0]), f32::from(h).max(MIN_SIZE[1])],
        None => DEFAULT_SIZE,
    }
}

/// The shell's per-frame part, separate from eframe so tests can drive it.
pub struct Shell {
    link: Link,
    layer: Box<dyn Layer>,
    nav: Option<Nav>,
    asked: Option<Page>,
    opened: Instant,
    /// The size last told to the daemon, and a newer one waiting to settle.
    size_told: Option<[u16; 2]>,
    size_seen: Option<([u16; 2], Instant)>,
    /// Keyboard focus as last told to the daemon (it takes a new window as
    /// without).
    focused: bool,
    /// While the open still asks for the default size (`reassert_size`).
    reassert: Option<Reassert>,
    /// The minimum size last asked for (`fit_screen`), and whether the
    /// window was made to fit the monitor once.
    min_sent: Option<egui::Vec2>,
    fitted: bool,
}

impl Shell {
    pub fn new(link: Link, layer: Box<dyn Layer>, asked: Option<Page>, opened: Instant) -> Self {
        Shell {
            link,
            layer,
            nav: None,
            asked,
            opened,
            size_told: None,
            size_seen: None,
            focused: false,
            reassert: None,
            min_sent: None,
            fitted: false,
        }
    }

    /// Opened without a size of our own (none remembered yet): see
    /// `reassert_size`.
    pub fn with_default_size(mut self) -> Self {
        self.reassert = Some(Reassert {
            left: REASSERTS,
            first_frame: None,
            last: None,
        });
        self
    }

    /// KWin remembers each app's window size (Wayland and Xwayland alike)
    /// and maps a new window at it, over the size asked for at creation,
    /// and a size asked on the first frame loses to it too. So the first
    /// v1 window opened at 0.11's smaller one. On an open without a size
    /// of our own, while the window is smaller than the default, the
    /// default is asked again: at most `REASSERTS` times, `REASSERT_GAP`
    /// apart, within `REASSERT_FOR` of the first frame. Once the window is
    /// the default size after one of them, or the time or the tries are
    /// up, it stops for good: from then on the size is the user's (and
    /// `track_size` remembers it).
    fn reassert_size(&mut self, ctx: &egui::Context) {
        let Some(re) = self.reassert.as_mut() else {
            return;
        };
        let now = Instant::now();
        let first = *re.first_frame.get_or_insert(now);
        let size = ctx.content_rect().size();
        let want = fit_to(monitor(ctx), DEFAULT_SIZE);
        let small = size.x < want.x - 1.0 || size.y < want.y - 1.0;
        let took = !small && re.last.is_some();
        if took || re.left == 0 || now.duration_since(first) >= REASSERT_FOR {
            self.reassert = None;
            return;
        }
        // Frames even without input, until the time is up.
        ctx.request_repaint_after(REASSERT_GAP);
        if !small
            || re
                .last
                .is_some_and(|t| now.duration_since(t) < REASSERT_GAP)
        {
            return;
        }
        re.left -= 1;
        re.last = Some(now);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
    }

    /// The window fits the monitor (UX panel A9): the minimum size never
    /// exceeds it less [`SCREEN_ROOM`] (asked again when the monitor or
    /// the text scale changes it), and a window that opened larger than
    /// that is brought down to it once.
    fn fit_screen(&mut self, ctx: &egui::Context) {
        let Some(m) = monitor(ctx) else {
            return;
        };
        let min = fit_to(Some(m), MIN_SIZE);
        if self.min_sent != Some(min) {
            self.min_sent = Some(min);
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min));
        }
        if !self.fitted {
            self.fitted = true;
            let size = ctx.content_rect().size();
            let fit = fit_to(Some(m), [size.x, size.y]);
            if fit != size {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(fit));
            }
        }
    }

    /// Remember a resize with the daemon once it settles.
    fn track_size(&mut self, ctx: &egui::Context) {
        let r = ctx.content_rect().size();
        let now = [r.x.round() as u16, r.y.round() as u16];
        if self.size_told.is_none() {
            // The size the window opened at is the daemon's already.
            self.size_told = Some(now);
            return;
        }
        if self.size_told == Some(now) {
            self.size_seen = None;
            return;
        }
        match self.size_seen {
            Some((s, at)) if s == now => {
                if at.elapsed() >= SIZE_SETTLE {
                    self.size_told = Some(now);
                    self.size_seen = None;
                    self.link.send(Action::UiSize(now[0], now[1]));
                } else {
                    ctx.request_repaint_after(SIZE_SETTLE - at.elapsed());
                }
            }
            _ => {
                self.size_seen = Some((now, Instant::now()));
                ctx.request_repaint_after(SIZE_SETTLE);
            }
        }
    }

    /// Tell the daemon when the window gains or loses keyboard focus (a
    /// focus change always brings a frame): while it has it, a problem the
    /// window shows is not also a desktop notification.
    fn track_focus(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        if focused != self.focused {
            self.focused = focused;
            self.link.send(Action::UiFocused(focused));
        }
    }

    /// The page shown (None before the first frame).
    pub fn page(&self) -> Option<Page> {
        self.nav.map(|n| n.page)
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    pub fn frame(&mut self, ctx: &egui::Context) {
        let nav = match self.nav.as_mut() {
            Some(nav) => nav,
            None => {
                // The first frame: wait for the daemon's first word (what
                // is left of the hold), then open on the page asked for,
                // else a fresh open's (first run while under way, else Live).
                let state = self
                    .link
                    .wait_first(FIRST_FRAME_HOLD.saturating_sub(self.opened.elapsed()));
                let page = self
                    .asked
                    .or_else(|| {
                        state.map(|s| {
                            let files = crate::pages::files::at_stake(&s) > 0;
                            crate::windowstate::fresh_page(&s.ui, files)
                        })
                    })
                    .unwrap_or_default();
                self.nav.insert(Nav { page })
            }
        };
        let before = nav.page;
        self.layer.ui(ctx, &self.link, nav);
        if nav.page != before {
            self.link.send(Action::UiPage(nav.page));
        }
        self.fit_screen(ctx);
        self.reassert_size(ctx);
        self.track_size(ctx);
        self.track_focus(ctx);
    }
}

struct App(Shell, dnd::Drops);

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !FIRST_FRAME.swap(true, Ordering::Relaxed) {
            quiet_xlib_io_errors();
        }
        self.0.frame(ctx);
    }

    /// Wayland drops join the input as X11's do (egui-winit's).
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.1.feed(raw);
    }
}

/// The window process's exit code when it could not open at all (no
/// display, no OpenGL): it never reached its first frame. The daemon says
/// so in a notification. Its own code: Xlib's `exit(1)` on a lost X server
/// or a panic's 101 is a window that was up.
pub const EXIT_COULD_NOT_OPEN: i32 = 4;
/// The exit code when its display went away after it was up (a logout, an
/// X server or compositor ending): the daemon only logs it.
pub const EXIT_DISPLAY_GONE: i32 = 3;

/// The window reached its first frame: from then on a failure is its
/// display going away, not a window that could not open.
static FIRST_FRAME: AtomicBool = AtomicBool::new(false);

pub fn first_frame_shown() -> bool {
    FIRST_FRAME.load(Ordering::Relaxed)
}

/// The exit code for how [`run`] ended.
pub fn exit_code(result: &Result<(), String>, first_frame: bool) -> i32 {
    match result {
        Ok(()) => 0,
        Err(_) if first_frame => EXIT_DISPLAY_GONE,
        Err(_) => EXIT_COULD_NOT_OPEN,
    }
}

/// A panic in the windowing layers after the first frame is the display
/// going away (winit panics on a lost X11 connection, the GL layers on a
/// lost surface): `file` is where it panicked. Anything else is a bug and
/// panics as usual.
pub fn display_gone_panic(file: &str, first_frame: bool) -> bool {
    const LAYERS: [&str; 10] = [
        "winit",
        "glutin",
        "glutin-winit",
        "glutin_egl_sys",
        "glutin_glx_sys",
        "x11rb",
        "x11-dl",
        "wayland-client",
        "wayland-backend",
        "smithay-client-toolkit",
    ];
    // registry and vendored paths: `winit-0.30.13/src/…`, `vendor/winit/…`
    first_frame
        && file.split(['/', '\\']).any(|part| {
            LAYERS.iter().any(|l| {
                part == *l
                    || part.strip_prefix(l).is_some_and(|rest| {
                        rest.starts_with('-') && rest[1..].starts_with(|c: char| c.is_ascii_digit())
                    })
            })
        })
}

/// Xlib's own answer to a lost X server is a message and `exit(1)`: once
/// the window is up (libX11 is loaded then when it is on X11; winit sets no
/// handler of its own), leave with [`EXIT_DISPLAY_GONE`] like the rest.
fn quiet_xlib_io_errors() {
    type Handler = unsafe extern "C" fn(*mut libc::c_void) -> libc::c_int;
    unsafe extern "C" fn gone(_display: *mut libc::c_void) -> libc::c_int {
        // errno says why (a broken pipe or reset is the server gone;
        // anything else, e.g. EAGAIN, is worth a look in the log)
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let mut line = [0u8; 96];
        let len = xlib_gone_line(errno, &mut line);
        // Plain write(2) inside Xlib's callback; Xlib must not get control
        // back (it would exit(1) itself), and nothing at exit may talk to
        // the dead connection.
        unsafe {
            libc::write(2, line.as_ptr().cast(), len);
            libc::_exit(EXIT_DISPLAY_GONE)
        }
    }
    // SAFETY: RTLD_NOLOAD only finds a libX11 already loaded (and keeps
    // it loaded), and XSetIOErrorHandler has this signature in every Xlib.
    unsafe {
        let lib = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        if lib.is_null() {
            return;
        }
        let set = libc::dlsym(lib, c"XSetIOErrorHandler".as_ptr());
        if !set.is_null() {
            let set: unsafe extern "C" fn(Option<Handler>) -> Option<Handler> =
                std::mem::transmute(set);
            set(Some(gone));
        }
    }
}

/// The Xlib handler's log line, with errno, into `buf` (no allocation);
/// returns its length.
fn xlib_gone_line(errno: i32, buf: &mut [u8; 96]) -> usize {
    const HEAD: &[u8] = b"[hushmic] the window's display went away (the X connection broke, errno ";
    let mut len = HEAD.len();
    buf[..len].copy_from_slice(HEAD);
    let mut digits = [0u8; 11];
    let mut n = errno.unsigned_abs();
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if errno < 0 {
        i -= 1;
        digits[i] = b'-';
    }
    let tail = &digits[i..];
    buf[len..len + tail.len()].copy_from_slice(tail);
    len += tail.len();
    buf[len..len + 2].copy_from_slice(b")\n");
    len + 2
}

/// What a panic in the window process means for its exit: the code to
/// leave with at once, or None to unwind as usual. Its display going away
/// ([`display_gone_panic`]) is [`EXIT_DISPLAY_GONE`]; a panic on the main
/// thread before the first frame (winit failing at startup) is a window
/// that could not open.
pub fn panic_exit(file: &str, first_frame: bool, main_thread: bool) -> Option<i32> {
    if display_gone_panic(file, first_frame) {
        Some(EXIT_DISPLAY_GONE)
    } else if !first_frame && main_thread {
        Some(EXIT_COULD_NOT_OPEN)
    } else {
        None
    }
}

/// In the window process: panics leave with the window's exit codes
/// ([`panic_exit`]): quietly when the display went away, with the panic
/// report when the window could not open.
pub fn set_panic_exits() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let (file, line) = info.location().map_or(("", 0), |l| (l.file(), l.line()));
        let main = std::thread::current().name() == Some("main");
        match panic_exit(file, first_frame_shown(), main) {
            Some(EXIT_DISPLAY_GONE) => {
                let said = info.payload_as_str().unwrap_or("(no message)");
                eprintln!("[hushmic] the window's display went away ({file}:{line}: {said})");
                // nothing at exit may talk to the dead display
                unsafe { libc::_exit(EXIT_DISPLAY_GONE) }
            }
            Some(code) => {
                default(info);
                unsafe { libc::_exit(code) }
            }
            None => default(info),
        }
    }));
}

/// A board state, in the look the window was asked for (`--theme`,
/// `--reduced-motion` stand in for the desktop), so a mock window stays in
/// it instead of turning to the mock's dark at its first snapshot.
fn mock_state(sc: Scenario, opts: &Options) -> crate::appstate::AppState {
    let mut state = crate::appstate::mock::state(sc);
    if let Some(theme) = opts.theme {
        state.desktop.system_theme = Some(theme);
        state.desktop.effective_theme = Theme::resolve(state.prefs.appearance, Some(theme));
    }
    state.desktop.reduced_motion |= opts.reduced_motion;
    state
}

/// Open the window and block until it closes.
pub fn run(opts: Options, make: MakeLayer) -> Result<(), String> {
    let opened = Instant::now();
    // The link connects before the window exists (the GL setup and the
    // daemon's first snapshot overlap); its wake-ups reach the window once
    // there is one.
    let ctx_cell: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
    let wake = {
        let cell = ctx_cell.clone();
        move || {
            if let Some(ctx) = cell.get() {
                // Not request_repaint(): from another thread that paints
                // twice.
                ctx.request_repaint_after(Duration::from_millis(1));
            }
        }
    };
    // A sandbox's folder is shown by its host path once the portal says.
    let _woken = crate::files::docpath::on_answer({
        let cell = ctx_cell.clone();
        move || {
            if let Some(ctx) = cell.get() {
                ctx.request_repaint_after(Duration::from_millis(1));
            }
        }
    });
    let (link, asked) = match opts.mock {
        Some(sc) => {
            let state = mock_state(sc, &opts);
            let page = opts.page.unwrap_or(state.ui.page);
            (Link::mock(state, wake), Some(page))
        }
        None => (Link::daemon(wake), opts.page),
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("HushMic")
        .with_app_id("hushmic")
        .with_inner_size(open_size(opts.size))
        .with_min_inner_size(MIN_SIZE)
        .with_resizable(true);
    if let Some(icon) = crate::branding::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    // No size of our own yet: ask for the default again after the map.
    let remembered = opts.size.is_some();
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    // app_name is eframe's storage identity, not the title: fixed.
    eframe::run_native(
        "hushmic-window",
        options,
        Box::new(move |cc| {
            let _ = ctx_cell.set(cc.egui_ctx.clone());
            // Portal dialogs open over this window from now on.
            parent::set_from(cc);
            // the X11 instance egui leaves empty
            wmclass::set(cc);
            let layer = make(cc, &opts);
            let drops = dnd::Drops::start(&cc.egui_ctx);
            let shell = Shell::new(link, layer, asked, opened);
            let shell = if remembered {
                shell
            } else {
                shell.with_default_size()
            };
            Ok(Box::new(App(shell, drops)))
        }),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_mock_window_keeps_the_look_it_was_asked_for() {
        let plain = mock_state(Scenario::OnInCall, &Options::default());
        assert_eq!(plain.desktop.effective_theme, Theme::Dark);
        assert!(!plain.desktop.reduced_motion);
        let opts = Options {
            theme: Some(Theme::Light),
            reduced_motion: true,
            ..Options::default()
        };
        let s = mock_state(Scenario::OnInCall, &opts);
        assert_eq!(s.desktop.system_theme, Some(Theme::Light));
        assert_eq!(s.desktop.effective_theme, Theme::Light);
        assert!(s.desktop.reduced_motion);
        // and SetAppearance still resolves against it
        let mut s = s;
        crate::appstate::mock::apply(
            &mut s,
            &Action::SetAppearance(crate::config::Appearance::Dark),
        );
        assert_eq!(s.desktop.effective_theme, Theme::Dark);
        crate::appstate::mock::apply(
            &mut s,
            &Action::SetAppearance(crate::config::Appearance::System),
        );
        assert_eq!(s.desktop.effective_theme, Theme::Light);
    }

    #[test]
    fn options_parse() {
        assert_eq!(parse_args(&[]), Ok(Options::default()));
        assert_eq!(
            parse_args(&words("--page files --mock muted --size 1024x700")),
            Ok(Options {
                page: Some(Page::Files),
                mock: Some(Scenario::Muted),
                size: Some([1024, 700]),
                ..Options::default()
            })
        );
        // Mic check is part of Live
        assert_eq!(
            parse_args(&words("--page mic-check")),
            Ok(Options {
                page: Some(Page::Live),
                ..Options::default()
            })
        );
        assert_eq!(
            parse_args(&words("--page live --theme light --reduced-motion")),
            Ok(Options {
                page: Some(Page::Live),
                theme: Some(Theme::Light),
                reduced_motion: true,
                ..Options::default()
            })
        );
        for bad in [
            "--page",
            "--page nowhere",
            "--mock",
            "--mock nope",
            "--frob",
            "--size",
            "--size 1024",
            "--size 1024xbig",
            "--theme",
            "--theme sepia",
        ] {
            assert!(parse_args(&words(bad)).is_err(), "{bad}");
        }
    }

    /// A layer that moves to Settings on its second frame and counts what
    /// it was shown.
    struct Probe {
        frames: Arc<std::sync::Mutex<Vec<Page>>>,
    }

    impl Layer for Probe {
        fn ui(&mut self, ctx: &egui::Context, link: &Link, nav: &mut Nav) {
            let mut f = self.frames.lock().unwrap();
            f.push(nav.page);
            if f.len() == 2 {
                nav.page = Page::Settings;
            }
            assert!(link.state().is_some());
            egui::CentralPanel::default().show(ctx, |_| {});
        }
    }

    fn shell(link: Link, asked: Option<Page>) -> (Shell, Arc<std::sync::Mutex<Vec<Page>>>) {
        let frames = Arc::new(std::sync::Mutex::new(Vec::new()));
        let layer = Box::new(Probe {
            frames: frames.clone(),
        });
        (Shell::new(link, layer, asked, Instant::now()), frames)
    }

    /// Opened without a page (a fresh open): Live, never the page the
    /// daemon last saw (the daemon passes `--page` when it re-creates).
    #[test]
    fn the_shell_opens_on_live_and_reports_moves() {
        let mut state = mock::state(Scenario::OnInCall);
        state.ui.page = Page::Files;
        let link = Link::recording(state);
        let (mut sh, frames) = shell(link, None);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |ctx| sh.frame(ctx));
        }
        assert_eq!(
            *frames.lock().unwrap(),
            vec![Page::Live, Page::Live, Page::Settings]
        );
        assert_eq!(sh.page(), Some(Page::Settings));
        // (a default RawInput has focus, which is told too)
        let moves: Vec<Action> = sh
            .link()
            .sent()
            .into_iter()
            .filter(|a| !matches!(a, Action::UiFocused(_)))
            .collect();
        assert_eq!(moves, vec![Action::UiPage(Page::Settings)]);
    }

    #[test]
    fn a_page_asked_for_wins_and_first_run_reopens_first_run() {
        let link = Link::recording(mock::state(Scenario::OnInCall));
        let (mut sh, _) = shell(link, Some(Page::Files));
        let _ = egui::Context::default().run(Default::default(), |ctx| sh.frame(ctx));
        assert_eq!(sh.page(), Some(Page::Files));
        let link = Link::recording(mock::state(Scenario::FirstRun2));
        let (mut sh, _) = shell(link, None);
        let _ = egui::Context::default().run(Default::default(), |ctx| sh.frame(ctx));
        assert_eq!(sh.page(), Some(Page::FirstRun));
    }

    #[test]
    fn the_open_size_is_remembered_but_never_under_the_minimum() {
        assert_eq!(open_size(None), DEFAULT_SIZE);
        assert_eq!(open_size(Some([1200, 800])), [1200.0, 800.0]);
        assert_eq!(open_size(Some([300, 200])), MIN_SIZE);
    }

    #[test]
    fn a_window_that_was_up_did_not_fail_to_open() {
        assert_eq!(exit_code(&Ok(()), false), 0);
        assert_eq!(exit_code(&Ok(()), true), 0);
        assert_eq!(
            exit_code(&Err("no display".into()), false),
            EXIT_COULD_NOT_OPEN
        );
        assert_eq!(
            exit_code(&Err("connection lost".into()), true),
            EXIT_DISPLAY_GONE
        );
    }

    #[test]
    fn a_windowing_panic_after_the_first_frame_is_the_display_going() {
        let x11 = "/b/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/winit-0.30.13/src/platform_impl/linux/x11/mod.rs";
        let vendored = "/build/hushmic/vendor/winit/src/platform_impl/linux/wayland/mod.rs";
        let sctk = "/c/smithay-client-toolkit-0.19.2/src/shell/xdg/window/mod.rs";
        for f in [x11, vendored, sctk] {
            assert!(display_gone_panic(f, true), "{f}");
            assert!(!display_gone_panic(f, false), "before the first frame: {f}");
        }
        // our own code, or a crate merely named alike, is a bug
        assert!(!display_gone_panic(
            "crates/hushmic/src/window/mod.rs",
            true
        ));
        assert!(!display_gone_panic("/c/winit-helpers/src/lib.rs", true));
        assert!(!display_gone_panic("/c/egui-0.33.3/src/context.rs", true));
    }

    #[test]
    fn the_xlib_line_says_errno() {
        let mut buf = [0u8; 96];
        let len = xlib_gone_line(104, &mut buf);
        assert_eq!(
            std::str::from_utf8(&buf[..len]).unwrap(),
            "[hushmic] the window's display went away (the X connection broke, errno 104)\n"
        );
        let len = xlib_gone_line(0, &mut buf);
        assert!(std::str::from_utf8(&buf[..len])
            .unwrap()
            .ends_with("errno 0)\n"));
        let len = xlib_gone_line(i32::MIN, &mut buf);
        assert!(std::str::from_utf8(&buf[..len])
            .unwrap()
            .ends_with("errno -2147483648)\n"));
    }

    #[test]
    fn a_panic_before_the_first_frame_could_not_open() {
        let winit = "/c/winit-0.30.13/src/platform_impl/linux/x11/mod.rs";
        let ours = "crates/hushmic/src/window/mod.rs";
        // up, then the display went
        assert_eq!(panic_exit(winit, true, true), Some(EXIT_DISPLAY_GONE));
        // winit (or anything) failing on the main thread at startup
        assert_eq!(panic_exit(winit, false, true), Some(EXIT_COULD_NOT_OPEN));
        assert_eq!(panic_exit(ours, false, true), Some(EXIT_COULD_NOT_OPEN));
        // a bug once up, or on another thread: a panic as usual
        assert_eq!(panic_exit(ours, true, true), None);
        assert_eq!(panic_exit(ours, false, false), None);
    }

    #[test]
    fn focus_gained_and_lost_is_told_once_each() {
        let link = Link::recording(mock::state(Scenario::OnInCall));
        let (mut sh, _) = shell(link, None);
        let ctx = egui::Context::default();
        let run = |sh: &mut Shell, focused: bool| {
            let input = egui::RawInput {
                focused,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| sh.frame(ctx));
        };
        let told = |sh: &Shell| -> Vec<Action> {
            sh.link()
                .sent()
                .into_iter()
                .filter(|a| matches!(a, Action::UiFocused(_)))
                .collect()
        };
        // Without focus from the start (the daemon assumes it): nothing.
        run(&mut sh, false);
        assert!(told(&sh).is_empty());
        run(&mut sh, true);
        run(&mut sh, true);
        assert_eq!(told(&sh), vec![Action::UiFocused(true)]);
        run(&mut sh, false);
        assert_eq!(
            told(&sh),
            vec![Action::UiFocused(true), Action::UiFocused(false)]
        );
    }

    #[test]
    fn a_settled_resize_is_told_once() {
        let link = Link::recording(mock::state(Scenario::OnInCall));
        let (mut sh, _) = shell(link, None);
        let ctx = egui::Context::default();
        let run = |sh: &mut Shell, w: f32| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(w, 640.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| sh.frame(ctx));
        };
        run(&mut sh, 960.0);
        run(&mut sh, 1000.0);
        run(&mut sh, 1100.0);
        assert!(!sh
            .link()
            .sent()
            .iter()
            .any(|a| matches!(a, Action::UiSize(..))));
        sh.size_seen = sh.size_seen.map(|(s, at)| (s, at - SIZE_SETTLE));
        run(&mut sh, 1100.0);
        run(&mut sh, 1100.0);
        let sizes: Vec<_> = sh
            .link()
            .sent()
            .into_iter()
            .filter(|a| matches!(a, Action::UiSize(..)))
            .collect();
        assert_eq!(sizes, vec![Action::UiSize(1100, 640)]);
    }

    #[test]
    fn the_window_never_outgrows_the_monitor() {
        // a 1280x800 screen at Xft.dpi 144 (1.5x): 853x533 points
        let small = Some(egui::vec2(1280.0 / 1.5, 800.0 / 1.5));
        let min = fit_to(small, MIN_SIZE);
        assert_eq!(min.x, MIN_SIZE[0], "the width fits");
        assert!(min.y + SCREEN_ROOM[1] <= 800.0 / 1.5 + 0.01, "{min:?}");
        // a large monitor, or none known: the minimum as designed
        assert_eq!(
            fit_to(Some(egui::vec2(2560.0, 1440.0)), MIN_SIZE),
            egui::vec2(720.0, 520.0)
        );
        assert_eq!(fit_to(None, MIN_SIZE), egui::vec2(720.0, 520.0));
        // the shell asks for it once, and brings a too-large open down
        let link = Link::recording(mock::state(Scenario::OnInCall));
        let (mut sh, _) = shell(link, None);
        let ctx = egui::Context::default();
        let run = |sh: &mut Shell, monitor: Option<egui::Vec2>| {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 640.0),
                )),
                ..Default::default()
            };
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .monitor_size = monitor;
            let out = ctx.run(input, |ctx| sh.frame(ctx));
            out.viewport_output
                .values()
                .flat_map(|v| v.commands.clone())
                .filter(|c| {
                    matches!(
                        c,
                        egui::ViewportCommand::MinInnerSize(_)
                            | egui::ViewportCommand::InnerSize(_)
                    )
                })
                .collect::<Vec<_>>()
        };
        assert!(run(&mut sh, None).is_empty(), "no monitor yet: nothing");
        let asked = run(&mut sh, small);
        assert!(
            asked.contains(&egui::ViewportCommand::MinInnerSize(min)),
            "{asked:?}"
        );
        assert!(
            asked.contains(&egui::ViewportCommand::InnerSize(fit_to(
                small,
                [960.0, 640.0]
            ))),
            "{asked:?}"
        );
        assert!(run(&mut sh, small).is_empty(), "once");
    }

    /// The InnerSize requests a frame of `w × h` content sends.
    fn size_asks(sh: &mut Shell, ctx: &egui::Context, w: f32, h: f32) -> usize {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(w, h),
            )),
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| sh.frame(ctx));
        out.viewport_output
            .values()
            .flat_map(|v| &v.commands)
            .filter(|c| {
                *c == &egui::ViewportCommand::InnerSize(egui::vec2(
                    DEFAULT_SIZE[0],
                    DEFAULT_SIZE[1],
                ))
            })
            .count()
    }

    /// Opened at another window's remembered 660 (KWin), with no size of
    /// our own: the default is asked again, a gap apart, at most 3 times,
    /// and once it took the user's own resizes are left alone.
    #[test]
    fn a_first_open_below_the_default_asks_for_it_again_then_lets_go() {
        let link = Link::recording(mock::state(Scenario::OnInCall));
        let (sh, _) = shell(link, None);
        let mut sh = sh.with_default_size();
        let ctx = egui::Context::default();
        assert_eq!(size_asks(&mut sh, &ctx, 660.0, 520.0), 1);
        // within the gap: not again
        assert_eq!(size_asks(&mut sh, &ctx, 660.0, 520.0), 0);
        let back = |sh: &mut Shell| {
            let re = sh.reassert.as_mut().unwrap();
            re.last = re.last.map(|t| t - REASSERT_GAP);
        };
        back(&mut sh);
        assert_eq!(
            size_asks(&mut sh, &ctx, 660.0, 520.0),
            1,
            "lost again: asked again"
        );
        back(&mut sh);
        assert_eq!(size_asks(&mut sh, &ctx, 960.0, 640.0), 0);
        assert!(sh.reassert.is_none(), "it took: done");
        // the user makes it smaller: theirs
        assert_eq!(size_asks(&mut sh, &ctx, 720.0, 520.0), 0);

        // a compositor that never gives it: 3 tries, then never again
        let (sh, _) = shell(Link::recording(mock::state(Scenario::OnInCall)), None);
        let mut sh = sh.with_default_size();
        let mut asked = 0;
        for _ in 0..6 {
            asked += size_asks(&mut sh, &ctx, 700.0, 600.0);
            if sh.reassert.is_some() {
                back(&mut sh);
            }
        }
        assert_eq!(asked, 3);
        assert!(sh.reassert.is_none());

        // and only in the first seconds after the first frame
        let (sh, _) = shell(Link::recording(mock::state(Scenario::OnInCall)), None);
        let mut sh = sh.with_default_size();
        assert_eq!(size_asks(&mut sh, &ctx, 960.0, 640.0), 0);
        let re = sh.reassert.as_mut().unwrap();
        re.first_frame = re.first_frame.map(|t| t - REASSERT_FOR);
        assert_eq!(
            size_asks(&mut sh, &ctx, 660.0, 520.0),
            0,
            "the user's size by now"
        );
        assert!(sh.reassert.is_none());
    }

    /// A size of our own (the daemon passes `--size`): never asked again.
    #[test]
    fn a_remembered_size_is_never_asked_again() {
        let link = Link::recording(mock::state(Scenario::OnInCall));
        let (mut sh, _) = shell(link, None);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            assert_eq!(size_asks(&mut sh, &ctx, 760.0, 560.0), 0);
        }
    }

    #[test]
    fn without_a_daemon_the_first_frame_waits_only_the_hold() {
        // A link that never gets a snapshot: the shell opens on Live after
        // the hold, never blocks longer.
        struct Never;
        impl Layer for Never {
            fn ui(&mut self, _: &egui::Context, _: &Link, _: &mut Nav) {}
        }
        let link = Link::daemon(|| {});
        if crate::control::socket_is_ours(&crate::lock::default_control_socket_path()) {
            return; // a real HushMic runs here
        }
        let mut sh = Shell::new(link, Box::new(Never), None, Instant::now());
        let t = Instant::now();
        let _ = egui::Context::default().run(Default::default(), |ctx| sh.frame(ctx));
        assert!(t.elapsed() < FIRST_FRAME_HOLD + Duration::from_millis(100));
        assert_eq!(sh.page(), Some(Page::Live));
    }
}
