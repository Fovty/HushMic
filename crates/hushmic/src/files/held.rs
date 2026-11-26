//! That a process holds something in the cache root (a `hushmic clean`
//! scratch folder, a note of a copy under way): an flock on a file, which
//! the kernel lets go when the process ends, however it ends. Unlike a pid
//! it means the same in every pid namespace: each Flatpak instance's first
//! process is pid 2, so two runs at once there share their pid.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Held while this lives (the file stays open, locked).
pub struct Held(File);

impl Held {
    /// The held file, to write into.
    pub fn file(&self) -> &File {
        &self.0
    }
}

/// Hold `path`, creating it. Waits while another process looks whether it
/// is held (a moment). A file a sweep removed between the open and the
/// lock is made again, so what is held is always the file at `path`.
pub fn hold(path: &Path) -> io::Result<Held> {
    for _ in 0..10 {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let here = std::fs::metadata(path);
        let mine = f.metadata()?;
        if here.is_ok_and(|m| m.dev() == mine.dev() && m.ino() == mine.ino()) {
            return Ok(Held(f));
        }
    }
    Err(io::Error::other(format!(
        "{} keeps going away",
        path.display()
    )))
}

/// Hold `path` if it is there and no process holds it, without waiting
/// (None: held, gone, or replaced meanwhile). What a sweep removes while
/// it holds this, no process uses: one that opened `path` meanwhile waits
/// in [`hold`], then finds the file gone and makes a new one. Looking with
/// [`is_held`] and then removing would not do: a process can take the
/// file in between and go on with one nobody can see any more.
pub fn take(path: &Path) -> Option<Held> {
    let f = File::open(path).ok()?;
    // SAFETY: flock on a descriptor this function owns.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return None;
    }
    let here = std::fs::metadata(path).ok()?;
    let mine = f.metadata().ok()?;
    (here.dev() == mine.dev() && here.ino() == mine.ino()).then_some(Held(f))
}

/// Whether a process holds `path` (no file: none does).
pub fn is_held(path: &Path) -> bool {
    let Ok(f) = File::open(path) else {
        return false;
    };
    // SAFETY: as above; the shared lock goes with `f`.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) != 0 }
}

/// A name no other run uses, in any pid namespace: `PREFIX` + pid + the
/// time in nanoseconds + a count.
pub fn unique(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{prefix}{}-{nanos:x}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_until_dropped_also_within_one_process() {
        let p = std::env::temp_dir().join(unique("hushmic-held-"));
        assert!(!is_held(&p), "no file");
        let h = hold(&p).unwrap();
        // another open file description: the same as another process
        assert!(is_held(&p));
        drop(h);
        assert!(!is_held(&p));
        // taken: held, and not to be taken again
        let t = take(&p).expect("free to take");
        assert!(is_held(&p) && take(&p).is_none());
        drop(t);
        let h = hold(&p).unwrap();
        assert!(take(&p).is_none(), "held by a run");
        drop(h);
        assert_ne!(unique("a-"), unique("a-"));
        let _ = std::fs::remove_file(&p);
        assert!(take(&p).is_none(), "no file");
    }
}
