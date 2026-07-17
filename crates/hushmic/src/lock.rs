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

/// Bind the show socket. Only the lock holder ever calls this, so a leftover
/// socket file is guaranteed dead (a crashed previous instance) — unlink it
/// first. In the /tmp fallback a foreign-uid squatter file makes the unlink
/// fail and the bind error out: the caller logs it and relaunch forwarding is
/// simply off, same failure posture as the lock's ownership check.
pub fn bind_show_socket(path: &Path) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    UnixListener::bind(path)
}

/// Ask the running instance to open its window. A completed connect IS the
/// whole message (no payload); false means nobody was listening — an old
/// hushmic without the socket, or a dead leftover socket file.
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
    UnixStream::connect(path).is_ok()
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
        assert!(
            listener.accept().is_ok(),
            "the ping arrives as one accepted connection"
        );

        // a dead socket file (crashed instance): connect is refused, and the
        // next holder's bind unlinks and rebinds over it
        drop(listener);
        assert!(!request_show(&path));
        let rebound = bind_show_socket(&path).expect("rebind over stale file");
        drop(rebound);
        let _ = std::fs::remove_file(&path);
    }
}
