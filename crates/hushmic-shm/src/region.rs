//! A shared file mapping that is only ever touched through atomics.
//!
//! Both sides of every layout in this crate run in different processes, so
//! the mapped bytes are accessed exclusively as `AtomicU32` / `AtomicU64`:
//! a racing access is then a well-defined (if possibly stale) value, never
//! undefined behaviour, and torn multi-field reads are caught by the
//! layouts' sequence checks. On x86-64 and aarch64 relaxed atomic loads and
//! stores compile to plain moves, so the copy loops cost what a `memcpy`
//! costs.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Mapping granularity the layouts are built on (headers are one page).
pub const PAGE: usize = 4096;

pub struct Region {
    ptr: *mut u8,
    len: usize,
    writable: bool,
    /// Device and inode of the mapped file, to tell whether a path still
    /// names it.
    id: (u64, u64),
    /// Kept open to count the file's names ([`Region::links`]).
    file: File,
}

// SAFETY: the region is plain shared memory, accessed only through atomics
// (see the module docs); the pointer stays valid until Drop.
unsafe impl Send for Region {}
// SAFETY: as above: every access is an atomic operation.
unsafe impl Sync for Region {}

impl Region {
    fn map(file: File, len: usize, writable: bool) -> io::Result<Region> {
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty mapping"));
        }
        let meta = file.metadata()?;
        let prot = if writable {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        // SAFETY: a fresh shared mapping of an open file descriptor; the
        // kernel picks the address. The file may be closed afterwards.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Region {
            ptr: ptr.cast(),
            len,
            writable,
            id: (meta.dev(), meta.ino()),
            file,
        })
    }

    /// Create `path` exclusively (mode 0600) at `len` bytes and map it
    /// read-write. Fails if anything exists at `path`.
    pub fn create_new(path: &Path, len: usize) -> io::Result<Region> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.set_len(len as u64)?;
        Region::map(file, len, true)
    }

    /// Open or create `path` (mode 0600) at exactly `len` bytes and map it
    /// read-write. An existing file must be a regular file owned by this
    /// user and private to it; a symlink is refused.
    pub fn create_or_open(path: &Path, len: usize) -> io::Result<Region> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        check_private(&file)?;
        if file.metadata()?.len() != len as u64 {
            file.set_len(len as u64)?;
        }
        Region::map(file, len, true)
    }

    /// Map an existing private file of exactly `len` bytes. Never blocks
    /// (a FIFO at `path` is refused, not waited on).
    pub fn open(path: &Path, len: usize, writable: bool) -> io::Result<Region> {
        Region::open_any(path, &[len], writable)
    }

    /// As [`Region::open`], for a file of any one of the sizes in `lens`
    /// (layouts of different versions); the mapping is the file's size.
    pub fn open_any(path: &Path, lens: &[usize], writable: bool) -> io::Result<Region> {
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        check_private(&file)?;
        let size = file.metadata()?.len();
        let Some(&len) = lens.iter().find(|&&l| l as u64 == size) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{size} bytes, expected one of {lens:?}"),
            ));
        };
        Region::map(file, len, writable)
    }

    /// How many names the mapped file has on its filesystem (0 once every
    /// one was removed).
    pub fn links(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.nlink())
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn writable(&self) -> bool {
        self.writable
    }

    /// `path` (not followed) still names the mapped file.
    pub fn is_at(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|m| (m.dev(), m.ino()) == self.id)
    }

    /// Remove `path` if it still names the mapped file; the mapping itself
    /// stays valid. Returns whether a name was removed.
    pub fn unlink_if_at(&self, path: &Path) -> bool {
        self.is_at(path) && std::fs::remove_file(path).is_ok()
    }

    pub(crate) fn u32(&self, off: usize) -> &AtomicU32 {
        assert!(off.is_multiple_of(4) && off + 4 <= self.len, "u32 at {off}");
        // SAFETY: in bounds and aligned (the mapping is page-aligned);
        // AtomicU32 has the layout of u32.
        unsafe { &*self.ptr.add(off).cast::<AtomicU32>() }
    }

    pub(crate) fn u64(&self, off: usize) -> &AtomicU64 {
        assert!(off.is_multiple_of(8) && off + 8 <= self.len, "u64 at {off}");
        // SAFETY: in bounds and aligned, as above.
        unsafe { &*self.ptr.add(off).cast::<AtomicU64>() }
    }

    pub(crate) fn u32s(&self, off: usize, n: usize) -> &[AtomicU32] {
        assert!(
            off.is_multiple_of(4) && off + 4 * n <= self.len,
            "{n} x u32 at {off}"
        );
        // SAFETY: in bounds and aligned, as above.
        unsafe { std::slice::from_raw_parts(self.ptr.add(off).cast::<AtomicU32>(), n) }
    }

    /// Fault every page in for writing without changing its contents, so
    /// the first real write never takes a page fault.
    pub fn prefault(&self) {
        assert!(self.writable, "prefault needs a writable mapping");
        for off in (0..self.len).step_by(PAGE) {
            self.u32(off).fetch_or(0, Ordering::Relaxed);
        }
    }

    /// Keep the pages in RAM where the memory-lock limit allows it.
    /// Best effort: `false` when the kernel refused.
    pub fn lock(&self) -> bool {
        // SAFETY: the range is exactly this live mapping.
        unsafe { libc::mlock(self.ptr.cast(), self.len) == 0 }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly the range mapped in `map`; no reference
        // into it outlives `self` (all accessors borrow `self`).
        unsafe {
            libc::munmap(self.ptr.cast(), self.len);
        }
    }
}

/// The file must be a regular file owned by us and closed to group and
/// others: the layouts carry the user's voice (the tap) and are trusted
/// for their sizes.
fn check_private(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    if meta.uid() != me {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "owned by another user",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "readable by other users",
        ));
    }
    Ok(())
}

/// Milliseconds on `CLOCK_MONOTONIC`: one clock for every process on the
/// machine, so the window's heartbeat can be aged by the plugin.
pub fn monotonic_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid output pointer; CLOCK_MONOTONIC always exists.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

/// Where tests keep their shared-memory files: `/dev/shm` when it is a
/// tmpfs (as a real `XDG_RUNTIME_DIR` is), else the temp dir. A fault on a
/// file mapping on a disk filesystem can wait for the disk on a busy
/// runner and hold up whichever test thread touches the page.
#[doc(hidden)]
pub fn test_dir_base() -> std::path::PathBuf {
    let shm = Path::new("/dev/shm");
    // SAFETY: a NUL-terminated path and a zeroed statfs to fill.
    let tmpfs = unsafe {
        let mut s: libc::statfs = std::mem::zeroed();
        libc::statfs(c"/dev/shm".as_ptr(), &mut s) == 0 && s.f_type == libc::TMPFS_MAGIC
    };
    if tmpfs && shm.is_dir() {
        shm.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

#[cfg(test)]
pub(crate) mod testdir {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh private directory per call (0700), removed by the caller.
    pub fn new(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = super::test_dir_base().join(format!(
            "hushmic-shm-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_symlinks_foreign_modes_and_wrong_sizes() {
        let d = testdir::new("region");
        let real = d.join("real");
        let r = Region::create_new(&real, PAGE).unwrap();
        r.u32(0).store(7, Ordering::Relaxed);
        drop(r);
        assert!(Region::create_new(&real, PAGE).is_err(), "exclusive");
        let ro = Region::open(&real, PAGE, false).unwrap();
        assert_eq!(ro.u32(0).load(Ordering::Relaxed), 7);
        assert!(!ro.writable());
        assert!(Region::open(&real, 2 * PAGE, false).is_err(), "wrong size");

        let link = d.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(Region::open(&link, PAGE, false).is_err(), "symlink");
        assert!(Region::create_or_open(&link, PAGE).is_err(), "symlink");

        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            Region::open(&real, PAGE, false).is_err(),
            "group/other readable"
        );
        assert!(Region::create_or_open(&real, PAGE).is_err());

        std::fs::create_dir(d.join("dir")).unwrap();
        assert!(Region::open(&d.join("dir"), PAGE, false).is_err());

        let fifo = d.join("fifo");
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid C path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(
            Region::open(&fifo, PAGE, false).is_err(),
            "a FIFO is refused, not waited on"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn counts_the_names_of_the_mapped_file() {
        let d = testdir::new("links");
        let p = d.join("f");
        let r = Region::create_new(&p, PAGE).unwrap();
        assert_eq!(r.links().unwrap(), 1);
        std::fs::hard_link(&p, d.join("g")).unwrap();
        assert_eq!(r.links().unwrap(), 2);
        assert!(r.unlink_if_at(&p));
        assert!(r.unlink_if_at(&d.join("g")));
        assert_eq!(r.links().unwrap(), 0);
        r.u32(0).store(1, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn create_or_open_resizes_and_prefaults() {
        let d = testdir::new("resize");
        let p = d.join("m");
        std::fs::write(&p, b"short").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let r = Region::create_or_open(&p, 2 * PAGE).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 2 * PAGE as u64);
        r.prefault();
        r.u64(PAGE).store(u64::MAX, Ordering::Relaxed);
        assert_eq!(r.u64(PAGE).load(Ordering::Relaxed), u64::MAX);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn monotonic_clock_moves_forward() {
        let a = monotonic_ms();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(monotonic_ms() >= a + 4);
    }
}
