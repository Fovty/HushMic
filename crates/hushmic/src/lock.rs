//! Single-instance guard + relaunch forwarding.
//!
//! hushmic must run as exactly one process per user session: a second
//! `hushmic --tray` would put up a second tray icon and spawn a second
//! `pipewire -c` filter-chain, both fighting to create/own `hushmic_source`
//! and the system default source. We take an advisory `flock` on a per-session
//! lock file; the kernel releases it automatically when the process exits (even
//! on crash/SIGKILL), so there is no stale-lock to clean up.
//!
//! Next to the lock lives a "show" socket: a plain `hushmic` launch that finds
//! the lock held connects to it and exits, and the running instance opens its
//! window — so clicking the app icon always does something visible.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// The runtime directory every instance of hushmic sees as the SAME directory.
/// Natively that is `$XDG_RUNTIME_DIR` itself. Inside a Flatpak each
/// `flatpak run` gets a private tmpfs at `$XDG_RUNTIME_DIR`; the one subtree
/// shared across instances of the same app is `app/$FLATPAK_ID`, so the lock
/// and the show socket must live there or two sandboxed launches simply
/// cannot see each other (each would take "the" lock and fight over
/// `hushmic_source`).
fn shared_runtime_dir() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    Some(match crate::sandbox::flatpak_app_id() {
        Some(id) => base.join("app").join(id),
        None => base,
    })
}

/// Default lock path (per-user, wiped on logout). The fallback lives in the
/// world-shared temp dir, so it is keyed by uid — a fixed name there could be
/// squatted by another local user (every launch would then silently exit
/// "already running") and would wrongly serialize two different users'
/// sessions against each other.
pub fn default_lock_path() -> PathBuf {
    match shared_runtime_dir() {
        Some(d) => d.join("hushmic.lock"),
        None => {
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("hushmic-{uid}.lock"))
        }
    }
}

/// Default path of the "show" socket, next to the lock file.
pub fn default_show_socket_path() -> PathBuf {
    match shared_runtime_dir() {
        Some(d) => d.join("hushmic-show.sock"),
        None => {
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("hushmic-show-{uid}.sock"))
        }
    }
}

/// Default path of the CLI control socket, next to the lock file (same
/// flatpak-shared-dir and /tmp-fallback reasoning as its siblings).
pub fn default_control_socket_path() -> PathBuf {
    match shared_runtime_dir() {
        Some(d) => d.join("hushmic-control.sock"),
        None => {
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("hushmic-control-{uid}.sock"))
        }
    }
}

/// Bind the show socket. Only the lock holder ever calls this, so a leftover
/// socket file is guaranteed dead (a crashed previous instance) — unlink it
/// first. In the /tmp fallback a foreign-uid squatter file makes the unlink
/// fail and the bind error out: the caller logs it and relaunch forwarding is
/// simply off, same failure posture as the lock's ownership check.
pub fn bind_show_socket(path: &Path) -> std::io::Result<UnixListener> {
    unlink_and_bind(path)
}

/// Bind the CLI control socket — identical stale-file posture: only the
/// lock holder binds, so any leftover file is a dead predecessor's.
pub fn bind_control_socket(path: &Path) -> std::io::Result<UnixListener> {
    unlink_and_bind(path)
}

fn unlink_and_bind(path: &Path) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    UnixListener::bind(path)
}

/// The display variables, and the desktop's name (the desktop reader of a
/// daemon the systemd unit started before the session needs it).
const DISPLAY_KEYS: [&str; 4] = [
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XAUTHORITY",
    "XDG_CURRENT_DESKTOP",
];

/// The one-line payload of a show request: the caller's display variables,
/// so a daemon started by `systemd --user` (whose environment has none)
/// can still open the window on the caller's screen. Values with
/// whitespace cannot be framed and are left out.
pub fn display_env_line() -> String {
    DISPLAY_KEYS
        .iter()
        .filter_map(|k| {
            let v = std::env::var(k).ok()?;
            (!v.is_empty() && !v.chars().any(char::is_whitespace)).then(|| format!("{k}={v}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Only those keys are accepted; anything else on the line is
/// dropped (the socket is per-user, but env is still not a thing to take
/// from a peer wholesale).
pub fn parse_display_env(line: &str) -> Vec<(String, String)> {
    line.split_whitespace()
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (DISPLAY_KEYS.contains(&k) && !v.is_empty()).then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// Ask the running instance to open its window: connect, send the display
/// line, done. False means nobody was listening — an old hushmic without
/// the socket, or a dead leftover socket file.
///
/// Mirrors `try_lock`'s ownership check before connecting: in the /tmp
/// fallback a squatter could pre-bind the predictable path, and connecting
/// to it would report "asked it to open the window" as a false success
/// (while handing the squatter a ping per launch). Requiring our own uid and
/// a real socket (no symlinks — `symlink_metadata` sees the link itself)
/// closes that; the sticky bit stops anyone else swapping the file after
/// the check.
pub fn request_show(path: &Path) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    match std::fs::symlink_metadata(path) {
        Ok(md) if md.uid() == unsafe { libc::getuid() } && md.file_type().is_socket() => {}
        _ => return false,
    }
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    // Best effort: a daemon that never reads the line (pre-0.8) still got
    // the connect, which is the message it understands.
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(1)));
    use std::io::Write;
    let _ = stream.write_all(format!("{}\n", display_env_line()).as_bytes());
    true
}

/// The daemon side of `request_show`: one line (bounded wait), parsed
/// leniently — an empty or unreadable line is a plain "show".
/// Bounded in both time (1 s per peer; the forwarding thread serves
/// peers one at a time, so a silent peer delays the next show by at most
/// that) and size (the three variables fit in far less than 4 KiB).
pub fn read_show_line(stream: &UnixStream) -> Vec<(String, String)> {
    use std::io::{BufRead, Read};
    if stream
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .is_err()
    {
        return Vec::new(); // cannot bound the read: plain show
    }
    let mut line = String::new();
    let _ = std::io::BufReader::new(stream.take(4096)).read_line(&mut line);
    parse_display_env(&line)
}

/// The exit code of a start that found another instance holding the lock
/// (`--tray`, `--headless`, `--enable-once`) outside systemd: nothing was
/// started. Units from 1.0 on do not restart on it either
/// (RestartPreventExitStatus), and under systemd the start exits 0.
pub const EXIT_ALREADY_RUNNING: i32 = 3;

/// Note in the held lock file who holds it (`PID MODE`: "1234 tray"), for
/// a second start to name in its "already running" line. Best effort.
pub fn record_holder(lock: &File, mode: &str) {
    use std::io::{Seek, Write};
    let mut f = lock;
    let _ = f.set_len(0);
    let _ = f.seek(std::io::SeekFrom::Start(0));
    let _ = writeln!(f, "{} {mode}", std::process::id());
}

/// The end of the holder's mode when the systemd unit started it ("tray,
/// systemd unit").
pub const HOLDER_UNIT: &str = ", systemd unit";

/// Who holds the lock at `path`, as [`record_holder`] noted it: (pid,
/// mode). None when nothing was noted (yet).
pub fn holder(path: &Path) -> Option<(u32, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let (pid, mode) = text.trim().split_once(' ')?;
    Some((pid.parse().ok()?, mode.to_string()))
}

/// What tells a unit's own process from one merely run beside systemd:
/// `INVOCATION_ID` is inherited, so a shell in a terminal whose server is
/// a systemd service (gnome-terminal-server) carries it too.
#[derive(Clone, Copy, Debug)]
pub struct Invocation {
    /// `INVOCATION_ID` is set.
    pub invocation_id: bool,
    /// `SYSTEMD_EXEC_PID` (systemd 248+: the pid it started), if set.
    pub exec_pid: Option<u32>,
    pub own_pid: u32,
    /// The parent is pid 1 or the user manager (`systemd`).
    pub parent_is_manager: bool,
    /// Inside a Flatpak, whose pid namespace hides both pids.
    pub flatpak: bool,
    /// stdin is a terminal.
    pub tty: bool,
}

impl Invocation {
    /// This process's own facts.
    pub fn here() -> Invocation {
        let ppid = std::os::unix::process::parent_id();
        let parent_is_manager = ppid == 1
            || std::fs::read_to_string(format!("/proc/{ppid}/comm"))
                .is_ok_and(|c| c.trim() == "systemd");
        Invocation {
            invocation_id: std::env::var_os("INVOCATION_ID").is_some(),
            exec_pid: std::env::var("SYSTEMD_EXEC_PID")
                .ok()
                .and_then(|p| p.trim().parse().ok()),
            own_pid: std::process::id(),
            parent_is_manager,
            flatpak: crate::sandbox::is_flatpak(),
            // SAFETY: isatty only reads the descriptor's state
            tty: unsafe { libc::isatty(0) } == 1,
        }
    }

    /// systemd started this very process: `SYSTEMD_EXEC_PID` is ours, or
    /// (older systemd) `INVOCATION_ID` is set and the manager is the
    /// parent. In a Flatpak neither pid can be compared (another pid
    /// namespace, `flatpak run` between), so there `INVOCATION_ID` and no
    /// terminal on stdin (a unit's stdin is /dev/null) decide.
    pub fn by_systemd(&self) -> bool {
        if self.flatpak {
            return self.invocation_id && !self.tty;
        }
        match self.exec_pid {
            Some(pid) => pid == self.own_pid,
            None => self.invocation_id && self.parent_is_manager,
        }
    }
}

/// The exit code and the line of a start that found another instance:
/// started by systemd ([`Invocation::by_systemd`]) 0, as before 1.0, so a unit any
/// version generated never restarts on it (`Restart=on-failure` leaves a
/// clean exit alone); elsewhere [`EXIT_ALREADY_RUNNING`], for scripts.
/// The hint fits the case (UX panel round 2, A11): two ways of starting at
/// login only when the one running is the systemd unit's
/// ([`HOLDER_UNIT`]); otherwise how to reach the one running.
pub fn already_running(under_systemd: bool, holder: Option<(u32, String)>) -> (i32, String) {
    let unit = holder
        .as_ref()
        .is_some_and(|(_, mode)| mode.ends_with(HOLDER_UNIT));
    let who = match holder {
        Some((pid, mode)) => format!(" (pid {pid}, {mode})"),
        None => String::new(),
    };
    if under_systemd {
        (
            0,
            format!("hushmic is already running{who}; this service instance stops."),
        )
    } else {
        let hint = if unit {
            "If both the login autostart entry and the systemd unit are enabled, disable one \
             of them."
        } else {
            "hushmic status shows what it does; hushmic quit stops it."
        };
        (
            EXIT_ALREADY_RUNNING,
            format!("hushmic is already running{who}. {hint}"),
        )
    }
}

/// Try to take the single-instance lock at `path`.
///
/// `Ok(Some(file))` — acquired; the caller MUST keep the returned `File` alive
/// for the whole process lifetime (dropping it releases the lock).
/// `Ok(None)` — another instance already holds it.
pub fn try_lock(path: &Path) -> std::io::Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    // The /tmp fallback lives in a world-writable dir: refuse a lock file
    // owned by another uid, or a squatter pre-holding the flock would make
    // every launch exit "already running" (success-coded, fully silent).
    {
        use std::os::unix::fs::MetadataExt;
        let md = file.metadata()?;
        let me = unsafe { libc::getuid() };
        if md.uid() != me {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "lock file {} is owned by uid {} (not us, uid {}) — remove it or set XDG_RUNTIME_DIR",
                    path.display(),
                    md.uid(),
                    me
                ),
            ));
        }
    }
    // LOCK_NB: fail fast instead of blocking behind the running instance.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(Some(file))
    } else {
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(None), // held by another instance
            _ => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_start_names_the_holder_and_exits_by_who_started_it() {
        let dir = std::env::temp_dir().join(format!("hushmic-holder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hushmic.lock");
        let held = try_lock(&path).unwrap().unwrap();
        assert_eq!(holder(&path), None, "nothing noted yet");
        record_holder(&held, "headless");
        record_holder(&held, "tray");
        let who = holder(&path);
        assert_eq!(
            who,
            Some((std::process::id(), "tray".into())),
            "the last word only"
        );
        // under systemd: 0, which Restart=on-failure never restarts
        let (code, line) = already_running(true, who.clone());
        assert_eq!(code, 0);
        assert_eq!(
            line,
            format!(
                "hushmic is already running (pid {}, tray); this service instance stops.",
                std::process::id()
            )
        );
        // a terminal or a script: 3, and how to reach the one running
        let (code, line) = already_running(false, None);
        assert_eq!(code, EXIT_ALREADY_RUNNING);
        assert_eq!(
            line,
            "hushmic is already running. hushmic status shows what it does; hushmic quit stops it."
        );
        // the unit's instance runs: the two login starts are the likely
        // cause (UX panel round 2, A11)
        let (code, line) = already_running(false, Some((7, format!("headless{HOLDER_UNIT}"))));
        assert_eq!(code, EXIT_ALREADY_RUNNING);
        assert_eq!(
            line,
            "hushmic is already running (pid 7, headless, systemd unit). If both the login \
             autostart entry and the systemd unit are enabled, disable one of them."
        );
        drop(held);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_units_own_process_counts_as_started_by_systemd() {
        let unit = Invocation {
            invocation_id: true,
            exec_pid: Some(4242),
            own_pid: 4242,
            parent_is_manager: true,
            flatpak: false,
            tty: false,
        };
        assert!(unit.by_systemd());
        // a shell under gnome-terminal-server.service: both inherited,
        // the pid is the terminal server's
        let terminal = Invocation {
            exec_pid: Some(1717),
            parent_is_manager: false,
            tty: true,
            ..unit
        };
        assert!(!terminal.by_systemd());
        // ... even with no terminal on stdin (a script run from it)
        assert!(!Invocation {
            tty: false,
            ..terminal
        }
        .by_systemd());
        // systemd before 248: the parent decides
        let old = Invocation {
            exec_pid: None,
            ..unit
        };
        assert!(old.by_systemd());
        assert!(!Invocation {
            parent_is_manager: false,
            ..old
        }
        .by_systemd());
        assert!(!Invocation {
            invocation_id: false,
            ..old
        }
        .by_systemd());
        // nothing from systemd at all
        assert!(!Invocation {
            invocation_id: false,
            exec_pid: None,
            parent_is_manager: false,
            ..unit
        }
        .by_systemd());
        // a Flatpak: the unit has no terminal, a typed start has one
        let flatpak = Invocation {
            flatpak: true,
            exec_pid: Some(1717),
            parent_is_manager: false,
            ..unit
        };
        assert!(flatpak.by_systemd());
        assert!(!Invocation {
            tty: true,
            ..flatpak
        }
        .by_systemd());
        assert!(!Invocation {
            invocation_id: false,
            ..flatpak
        }
        .by_systemd());
    }

    #[test]
    fn display_env_line_round_trips_and_ignores_junk() {
        let parsed = parse_display_env(
            "WAYLAND_DISPLAY=wayland-0 DISPLAY=:0 XAUTHORITY=/run/user/1000/xauth_ABC\n",
        );
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], ("WAYLAND_DISPLAY".into(), "wayland-0".into()));
        assert_eq!(parsed[2].1, "/run/user/1000/xauth_ABC");
        assert_eq!(
            parse_display_env("XDG_CURRENT_DESKTOP=KDE"),
            [("XDG_CURRENT_DESKTOP".to_string(), "KDE".to_string())]
        );
        assert!(parse_display_env("").is_empty());
        assert!(parse_display_env("\n").is_empty());
        assert!(parse_display_env("PATH=/evil LD_PRELOAD=/x").is_empty());
        assert_eq!(parse_display_env("DISPLAY=:1 garbage DISPLAY=").len(), 1);
        for (k, _) in parse_display_env(&display_env_line()) {
            assert!(DISPLAY_KEYS.contains(&k.as_str()));
        }
    }

    #[test]
    fn second_lock_is_refused_until_first_releases() {
        // flock is per open-file-description, so a second attempt on the same
        // path is refused even within one process — exactly the cross-process
        // behaviour the guard relies on.
        let path =
            std::env::temp_dir().join(format!("hushmic-locktest-{}.lock", std::process::id()));
        let first = try_lock(&path).expect("io ok");
        assert!(first.is_some(), "first lock should succeed");

        let second = try_lock(&path).expect("io ok");
        assert!(
            second.is_none(),
            "second lock must be refused while the first is held"
        );

        drop(first);
        let third = try_lock(&path).expect("io ok");
        assert!(
            third.is_some(),
            "lock should be re-acquirable after release"
        );

        drop(third);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn show_socket_pings_the_listener_and_survives_stale_files() {
        let path =
            std::env::temp_dir().join(format!("hushmic-socktest-{}.sock", std::process::id()));
        // nobody listening (no file at all) -> the request reports failure
        assert!(!request_show(&path));

        let listener = bind_show_socket(&path).expect("bind");
        assert!(request_show(&path), "connect to a live listener succeeds");
        let (conn, _) = listener
            .accept()
            .expect("the ping arrives as one accepted connection");
        assert_eq!(
            read_show_line(&conn),
            parse_display_env(&display_env_line()),
            "the show line carries the caller's display vars"
        );

        // a dead socket file (crashed instance): connect is refused, and the
        // next holder's bind unlinks and rebinds over it. Refused once
        // nobody holds the listener: a child another test's thread forked
        // a moment ago holds a copy of it until its exec, and a connect in
        // that moment is still taken.
        drop(listener);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while request_show(&path) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!request_show(&path), "a listener nobody holds refuses");
        let rebound = bind_show_socket(&path).expect("rebind over stale file");
        drop(rebound);
        let _ = std::fs::remove_file(&path);
    }
}
