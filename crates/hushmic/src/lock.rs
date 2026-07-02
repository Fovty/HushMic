//! Single-instance guard.
//!
//! hushmic must run as exactly one process per user session: a second
//! `hushmic --tray` would put up a second tray icon and spawn a second
//! `pipewire -c` filter-chain, both fighting to create/own `hushmic_source`
//! and the system default source. We take an advisory `flock` on a per-session
//! lock file; the kernel releases it automatically when the process exits (even
//! on crash/SIGKILL), so there is no stale-lock to clean up.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Default lock path: `$XDG_RUNTIME_DIR/hushmic.lock` (per-user, wiped on
/// logout). The fallback lives in the world-shared temp dir, so it is keyed by
/// uid — a fixed name there could be squatted by another local user (every
/// launch would then silently exit "already running") and would wrongly
/// serialize two different users' sessions against each other.
pub fn default_lock_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("hushmic.lock"),
        None => {
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("hushmic-{uid}.lock"))
        }
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
}
