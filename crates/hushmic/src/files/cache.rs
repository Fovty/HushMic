//! A file's cleaned sound on disk, in HushMic's cache folder: f32 frames
//! at 48 kHz, interleaved, and the file's picture (`super::picture`: the
//! spectrum of the original and of the cleaned sound, a few hundred
//! columns for the whole file). Writes are positioned: fronts fill
//! different parts of the file at once. The folder goes when the file
//! leaves the page; a stale one is swept at the next start.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::picture;
use crate::miccheck::spectrum::ROWS;

const CLEAN: &str = "clean.f32";

pub struct Cache {
    dir: PathBuf,
    clean: File,
    /// The picture, where this cache writes it (a reader of the sound
    /// has none: the window reads a picture whole, `picture::Sheet`).
    picture: Option<File>,
    /// Fronts overlap at a hand-over: one of them at a time puts a
    /// cleaned column.
    putting: Mutex<()>,
    channels: usize,
}

/// `$XDG_CACHE_HOME/hushmic/files` (inside a Flatpak, the app's own).
pub fn root() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.cache_dir().join("hushmic").join("files"))
}

/// [`root`], or a folder in the temp dir where there is no home: where
/// the daemon's jobs and `hushmic clean` keep their caches.
pub fn root_or_temp() -> PathBuf {
    root().unwrap_or_else(|| std::env::temp_dir().join("hushmic-files"))
}

/// Where the cleaned sound of a file's `track`th sound track goes (1: the
/// second; the first is the cache folder itself): a folder in it.
pub fn track_dir(cache_dir: &Path, track: usize) -> PathBuf {
    if track == 0 {
        cache_dir.to_path_buf()
    } else {
        cache_dir.join(format!("track-{track}"))
    }
}

/// Disk the cache of a file needs: the cleaned sound plus its picture.
pub fn bytes_needed(frames48: u64, channels: usize) -> u64 {
    frames48 * channels as u64 * 4 + picture::BYTES_MAX
}

/// Free bytes for an unprivileged writer on the filesystem holding `dir`.
pub fn free_bytes(dir: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(s.f_bavail as u64 * s.f_frsize as u64)
}

impl Cache {
    /// A new, empty cache in `dir` (created; an existing one is emptied).
    pub fn create(dir: &Path, channels: usize) -> io::Result<Cache> {
        std::fs::create_dir_all(dir)?;
        let open = |name: &str| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(dir.join(name))
        };
        Ok(Cache {
            dir: dir.to_path_buf(),
            clean: open(CLEAN)?,
            picture: Some(open(picture::NAME)?),
            putting: Mutex::new(()),
            channels: channels.max(1),
        })
    }

    /// An existing cache, to read (the window's player): what the worker
    /// writes shows through.
    pub fn open(dir: &Path, channels: usize) -> io::Result<Cache> {
        Ok(Cache {
            dir: dir.to_path_buf(),
            clean: File::open(dir.join(CLEAN))?,
            picture: None,
            putting: Mutex::new(()),
            channels: channels.max(1),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Write planar `block` starting at frame `at`.
    pub fn write(&self, at: u64, block: &[Vec<f32>]) -> io::Result<()> {
        let n = block.first().map_or(0, Vec::len);
        let mut bytes = Vec::with_capacity(n * self.channels * 4);
        for i in 0..n {
            for c in 0..self.channels {
                let v = block.get(c).or(block.first()).map_or(0.0, |ch| ch[i]);
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        self.clean
            .write_all_at(&bytes, at * self.channels as u64 * 4)
    }

    /// `frames` frames from `at`, planar; silence where nothing is written
    /// (past the end, or a part not cleaned yet).
    pub fn read(&self, at: u64, frames: usize) -> io::Result<Vec<Vec<f32>>> {
        let mut bytes = vec![0u8; frames * self.channels * 4];
        let mut got = 0;
        let start = at * self.channels as u64 * 4;
        while got < bytes.len() {
            let n = self.clean.read_at(&mut bytes[got..], start + got as u64)?;
            if n == 0 {
                break;
            }
            got += n;
        }
        let mut out = vec![Vec::with_capacity(frames); self.channels];
        for f in bytes.chunks_exact(self.channels * 4) {
            for (c, o) in out.iter_mut().enumerate() {
                let b = &f[c * 4..c * 4 + 4];
                o.push(f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            }
        }
        Ok(out)
    }

    /// Start the file's picture: columns of `span` frames.
    pub fn start_picture(&self, span: u64) -> io::Result<()> {
        match &self.picture {
            Some(f) => picture::start(f, span),
            None => Ok(()),
        }
    }

    /// A column of the picture's original lane (`picture::Original`).
    pub fn put_original(&self, col: u64, hops: u16, levels: &[u8; ROWS]) -> io::Result<()> {
        match &self.picture {
            Some(f) => picture::put_original(f, col, hops, levels),
            None => Ok(()),
        }
    }

    /// A column of its cleaned lane (`picture::Cleaned`); the fuller of
    /// two fronts' stays.
    pub fn put_cleaned(
        &self,
        col: u64,
        hops: u16,
        levels: &[u8; ROWS],
        like: &[i8; ROWS],
    ) -> io::Result<()> {
        let Some(f) = &self.picture else {
            return Ok(());
        };
        let _one = self.putting.lock().unwrap_or_else(|e| e.into_inner());
        picture::put_cleaned(f, col, hops, levels, like)
    }

    /// The picture's bytes as written so far.
    pub fn picture_bytes(&self) -> io::Result<Vec<u8>> {
        std::fs::read(self.dir.join(picture::NAME))
    }

    /// Delete the folder and everything in it.
    pub fn remove(self) -> io::Result<()> {
        let dir = self.dir.clone();
        drop(self);
        std::fs::remove_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("hushmic-cache-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn positioned_writes_read_back_and_gaps_are_silent() {
        let d = tmp("rw");
        let c = Cache::create(&d, 2).unwrap();
        c.write(10, &[vec![0.5, 0.25], vec![-0.5, -0.25]]).unwrap();
        c.write(0, &[vec![1.0], vec![-1.0]]).unwrap();
        let r = c.read(0, 14).unwrap();
        // A reader elsewhere sees the same.
        assert_eq!(Cache::open(&d, 2).unwrap().read(0, 14).unwrap(), r);
        assert_eq!(r[0][..3], [1.0, 0.0, 0.0]);
        assert_eq!(r[0][10..12], [0.5, 0.25]);
        assert_eq!(r[1][10..12], [-0.5, -0.25]);
        assert_eq!(r[0][13], 0.0, "past the end reads as silence");
        c.remove().unwrap();
        assert!(!d.exists());
    }

    #[test]
    fn the_picture_is_written_next_to_the_sound_and_goes_with_it() {
        let d = tmp("picture");
        let c = Cache::create(&d, 1).unwrap();
        c.start_picture(800).unwrap();
        c.put_original(2, 1, &[7; ROWS]).unwrap();
        c.put_cleaned(2, 1, &[5; ROWS], &[127; ROWS]).unwrap();
        let sheet = picture::Sheet::read(&d).unwrap().unwrap();
        assert_eq!((sheet.cols(), sheet.lit()), (3, 1));
        assert_eq!(picture::originals(&c.picture_bytes().unwrap()).count(), 1);
        // a reader of the sound writes none
        let r = Cache::open(&d, 1).unwrap();
        r.put_original(9, 1, &[7; ROWS]).unwrap();
        assert_eq!(picture::Sheet::read(&d).unwrap().unwrap().cols(), 3);
        c.remove().unwrap();
        assert!(!d.exists());
        assert_eq!(picture::Sheet::read(&d).unwrap(), None);
    }

    #[test]
    fn space_needed_and_free_space() {
        // An hour of stereo: 0.69 GB per channel.
        let h = 48_000 * 3600;
        assert_eq!(bytes_needed(h, 2), h * 8 + picture::BYTES_MAX);
        assert!(free_bytes(&std::env::temp_dir()).unwrap() > 0);
    }
}
