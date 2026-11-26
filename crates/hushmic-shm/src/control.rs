//! The chain control file: the mode and the strength the app wants, which
//! the plugin of HushMic's own chain takes every cycle in place of its
//! Mode and Attenuation Limit ports.
//!
//! The ports are written through PipeWire (`pw-cli set-param`), and every
//! pw-cli command waits for the session manager: while WirePlumber does
//! not answer, a mute written as a port waits with it. A store into this
//! page lands within one cycle whatever PipeWire, WirePlumber or the app's
//! main loop are doing.
//!
//! Layout v1 (one page, little endian, every field naturally aligned):
//!
//! | Offset | Field | Writer | Meaning |
//! |---|---|---|---|
//! | 0 | magic u64 | app | `HMCTRL\0\0`; written last at creation |
//! | 8 | version u32 | app | 1 |
//! | 12 | size u32 | app | 4096 |
//! | 16 | mode u32 | app | [`MODE_PROCESS`], [`MODE_BYPASS`], [`MODE_MUTE`] (the meter's codes) |
//! | 20 | attn_db u32 | app | f32 bits: the Attenuation Limit (dB), finite, [`ATTN_MIN`]..=[`ATTN_MAX`] |
//! | 24 | reader u32 | plugin | 0 until a plugin maps the file and will honour it, then the version it read |
//! | 28 | reserved | | zero; a later version appends fields and bumps `version` |
//!
//! Lifecycle: the app creates the file before it starts the chain (with
//! the values the chain's conf carries) and never resizes or replaces it
//! while that chain runs; it removes it with the chain. The plugin maps it
//! when it is instantiated, never while processing, and only if the magic,
//! the version and the size check out; otherwise it keeps using its ports,
//! exactly as a plugin in any other host does. A plugin that does not know
//! the file's version never sets `reader`, so the app knows it is ignored
//! and keeps writing the ports too.
//!
//! Values: only the three modes and a finite strength within the port's
//! range are valid. The writer refuses anything else; a reader reads an
//! invalid value as none, and the plugin then keeps the last valid value
//! it took from the file ([`LastValid`]), or its port while it has none.
//! An invalid word must never read as "process": if mute was meant, that
//! would open the microphone.
//!
//! The app's stores are single aligned words: a reader sees the old value
//! or the new one, never a mix. Reading is two relaxed loads, no system
//! call, from a page faulted in when it was mapped.

use crate::region::{Region, PAGE};
use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;

pub use crate::meter::{MODE_BYPASS, MODE_MUTE, MODE_PROCESS};

pub const MAGIC: u64 = u64::from_le_bytes(*b"HMCTRL\0\0");
/// The Attenuation Limit port's range (dB).
pub const ATTN_MIN: f32 = 0.0;
pub const ATTN_MAX: f32 = 100.0;
pub const VERSION: u32 = 1;
/// The control file is exactly one page.
pub const SIZE: usize = PAGE;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 8;
const OFF_SIZE: usize = 12;
const OFF_MODE: usize = 16;
const OFF_ATTN: usize = 20;
const OFF_READER: usize = 24;

fn valid_mode(mode: u32) -> Option<u32> {
    matches!(mode, MODE_PROCESS | MODE_BYPASS | MODE_MUTE).then_some(mode)
}

fn valid_attn(db: f32) -> Option<f32> {
    (db.is_finite() && (ATTN_MIN..=ATTN_MAX).contains(&db)).then_some(db)
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

/// The app's side: creates the file and writes what it wants.
pub struct ControlWriter {
    region: Region,
}

impl ControlWriter {
    /// Create the control file at `path` (exclusively, 0600) holding
    /// `mode` and `attn_db`. Fails if anything exists at `path`, or for a
    /// value that is not valid (see the module docs).
    pub fn create(path: &Path, mode: u32, attn_db: f32) -> io::Result<ControlWriter> {
        valid_mode(mode).ok_or_else(|| invalid("mode"))?;
        valid_attn(attn_db).ok_or_else(|| invalid("attn_db"))?;
        let region = Region::create_new(path, SIZE)?;
        region.u32(OFF_VERSION).store(VERSION, Ordering::Relaxed);
        region.u32(OFF_SIZE).store(SIZE as u32, Ordering::Relaxed);
        region.u32(OFF_MODE).store(mode, Ordering::Relaxed);
        region
            .u32(OFF_ATTN)
            .store(attn_db.to_bits(), Ordering::Relaxed);
        // Last: a reader that sees the magic sees everything above.
        region.u64(OFF_MAGIC).store(MAGIC, Ordering::Release);
        Ok(ControlWriter { region })
    }

    /// Store `mode`; an invalid one is refused (false, nothing written).
    pub fn set_mode(&self, mode: u32) -> bool {
        let Some(mode) = valid_mode(mode) else {
            return false;
        };
        self.region.u32(OFF_MODE).store(mode, Ordering::Release);
        true
    }

    /// Store `db`; an invalid one is refused (false, nothing written).
    pub fn set_attn_db(&self, db: f32) -> bool {
        let Some(db) = valid_attn(db) else {
            return false;
        };
        self.region
            .u32(OFF_ATTN)
            .store(db.to_bits(), Ordering::Release);
        true
    }

    pub fn mode(&self) -> u32 {
        self.region.u32(OFF_MODE).load(Ordering::Relaxed)
    }

    pub fn attn_db(&self) -> f32 {
        f32::from_bits(self.region.u32(OFF_ATTN).load(Ordering::Relaxed))
    }

    /// Whether a plugin has mapped the file and takes mode and strength
    /// from it (it wrote the version it read).
    pub fn honoured(&self) -> bool {
        self.region.u32(OFF_READER).load(Ordering::Acquire) == VERSION
    }

    /// Remove `path` if it still names this file; the mapping stays valid.
    pub fn unlink_if_at(&self, path: &Path) -> bool {
        self.region.unlink_if_at(path)
    }
}

/// The plugin's side: mapped once, read every cycle.
pub struct ControlReader {
    region: Region,
    locked: bool,
}

impl ControlReader {
    /// Map the control file at `path` if it is one this plugin can honour
    /// (magic, a known version, one page), fault its page in, lock it in
    /// RAM where the memory-lock limit allows ([`ControlReader::locked`]),
    /// and tell the app so (`reader`). Not for the audio thread: it opens
    /// and maps a file. An error means: use the ports.
    pub fn open(path: &Path) -> io::Result<ControlReader> {
        let region = Region::open(path, SIZE, true)?;
        if region.u64(OFF_MAGIC).load(Ordering::Acquire) != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a control file",
            ));
        }
        let version = region.u32(OFF_VERSION).load(Ordering::Relaxed);
        if version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("control file version {version}, this plugin reads {VERSION}"),
            ));
        }
        let size = region.u32(OFF_SIZE).load(Ordering::Relaxed);
        if size as usize != SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("control file says {size} bytes, expected {SIZE}"),
            ));
        }
        region.prefault();
        let locked = region.lock();
        region.u32(OFF_READER).store(VERSION, Ordering::Release);
        Ok(ControlReader { region, locked })
    }

    /// Whether the page is locked in RAM. False when the kernel refused
    /// (RLIMIT_MEMLOCK): the page can then be swapped out, and the audio
    /// thread's next read of it would wait for the disk.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// The mode code the app wants; None when the word is not a valid
    /// mode (keep the last valid one: [`LastValid`]). Wait-free.
    pub fn mode(&self) -> Option<u32> {
        valid_mode(self.region.u32(OFF_MODE).load(Ordering::Relaxed))
    }

    /// The Attenuation Limit (dB) the app wants; None when the word is not
    /// a valid strength. Wait-free.
    pub fn attn_db(&self) -> Option<f32> {
        valid_attn(f32::from_bits(
            self.region.u32(OFF_ATTN).load(Ordering::Relaxed),
        ))
    }
}

/// What the plugin applies from the file: the newest valid value, else the
/// last valid one, else none (its port). Plain state, no allocation: one
/// per instance, and as every instance reads the same page they agree.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LastValid {
    mode: Option<u32>,
    attn_db: Option<f32>,
}

impl LastValid {
    /// The mode to apply given what the file says now (`reader.mode()`);
    /// None: no valid file mode seen yet, use the port.
    pub fn mode(&mut self, file: Option<u32>) -> Option<u32> {
        if file.is_some() {
            self.mode = file;
        }
        self.mode
    }

    /// The strength counterpart of [`LastValid::mode`].
    pub fn attn_db(&mut self, file: Option<f32>) -> Option<f32> {
        if file.is_some() {
            self.attn_db = file;
        }
        self.attn_db
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::testdir;

    #[test]
    fn the_plugin_reads_what_the_app_writes_and_says_so() {
        let d = testdir::new("control");
        let p = d.join("control-1");
        let w = ControlWriter::create(&p, MODE_BYPASS, 24.0).unwrap();
        assert!(!w.honoured(), "nobody mapped it yet");
        let r = ControlReader::open(&p).unwrap();
        assert!(w.honoured());
        // Locked unless this process's memory-lock limit is used up (the
        // tap tests lock 4 MiB rings next to it): then the kernel says so.
        if r.locked() {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            assert!(!status.contains("VmLck:\t       0 kB"), "{status}");
        }
        assert_eq!((r.mode(), r.attn_db()), (Some(MODE_BYPASS), Some(24.0)));
        assert!(w.set_mode(MODE_MUTE));
        assert!(w.set_attn_db(12.5));
        assert_eq!((r.mode(), r.attn_db()), (Some(MODE_MUTE), Some(12.5)));
        assert_eq!((w.mode(), w.attn_db()), (MODE_MUTE, 12.5));
        // Unlinked with the chain: both mappings stay valid.
        assert!(w.unlink_if_at(&p));
        assert!(!p.exists());
        assert!(w.set_mode(MODE_PROCESS));
        assert_eq!(r.mode(), Some(MODE_PROCESS));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn only_a_file_this_plugin_knows_is_used() {
        let d = testdir::new("control-bad");
        let p = d.join("control-1");
        // Missing.
        assert!(ControlReader::open(&p).is_err());
        // Exists, but nobody wrote the magic.
        std::fs::write(&p, vec![0u8; SIZE]).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ControlReader::open(&p).is_err());
        std::fs::remove_file(&p).unwrap();
        // Short.
        std::fs::write(&p, MAGIC.to_le_bytes()).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ControlReader::open(&p).is_err());
        std::fs::remove_file(&p).unwrap();
        // A version from a newer app: ports, and `reader` stays 0 so the
        // app knows.
        let w = ControlWriter::create(&p, MODE_MUTE, 30.0).unwrap();
        w.region
            .u32(OFF_VERSION)
            .store(VERSION + 1, Ordering::Relaxed);
        let e = ControlReader::open(&p).err().unwrap();
        assert!(e.to_string().contains("version 2"), "{e}");
        assert!(!w.honoured());
        // A size field that disagrees.
        w.region.u32(OFF_VERSION).store(VERSION, Ordering::Relaxed);
        w.region.u32(OFF_SIZE).store(8192, Ordering::Relaxed);
        assert!(ControlReader::open(&p).is_err());
        assert!(!w.honoured());
        drop(w);
        std::fs::remove_file(&p).unwrap();
        // Never replaced: a second create at the same path fails.
        let _w = ControlWriter::create(&p, MODE_PROCESS, 0.0).unwrap();
        assert!(ControlWriter::create(&p, MODE_PROCESS, 0.0).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_invalid_value_never_unmutes() {
        let d = testdir::new("control-invalid");
        let p = d.join("control-1");
        // The writer refuses what the port could not carry.
        assert!(ControlWriter::create(&p, 3, 0.0).is_err());
        assert!(ControlWriter::create(&p, MODE_MUTE, f32::NAN).is_err());
        assert!(ControlWriter::create(&p, MODE_MUTE, 100.5).is_err());
        assert!(!p.exists(), "nothing created for an invalid value");
        let w = ControlWriter::create(&p, MODE_MUTE, 30.0).unwrap();
        for bad in [3, 7, u32::MAX] {
            assert!(!w.set_mode(bad));
        }
        for bad in [f32::NAN, f32::INFINITY, -0.5, 100.01] {
            assert!(!w.set_attn_db(bad));
        }
        assert_eq!(
            (w.mode(), w.attn_db()),
            (MODE_MUTE, 30.0),
            "nothing written"
        );
        // A word that is invalid anyway (a newer writer, a stray store):
        // the reader says none, and the last valid value stays in force.
        let r = ControlReader::open(&p).unwrap();
        let mut last = LastValid::default();
        assert_eq!(last.mode(r.mode()), Some(MODE_MUTE));
        assert_eq!(last.attn_db(r.attn_db()), Some(30.0));
        w.region.u32(OFF_MODE).store(9, Ordering::Relaxed);
        w.region
            .u32(OFF_ATTN)
            .store(f32::NAN.to_bits(), Ordering::Relaxed);
        assert_eq!((r.mode(), r.attn_db()), (None, None));
        assert_eq!(last.mode(r.mode()), Some(MODE_MUTE), "still muted");
        assert_eq!(last.attn_db(r.attn_db()), Some(30.0));
        assert!(w.set_mode(MODE_BYPASS));
        assert_eq!(last.mode(r.mode()), Some(MODE_BYPASS));
        // No valid file value seen yet: the port decides.
        let mut fresh = LastValid::default();
        assert_eq!(fresh.mode(None), None);
        assert_eq!(fresh.attn_db(None), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_layout_is_pinned() {
        let d = testdir::new("control-layout");
        let p = d.join("control-1");
        let _w = ControlWriter::create(&p, MODE_MUTE, 37.5).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(raw.len(), 4096);
        assert_eq!(&raw[0..8], b"HMCTRL\0\0");
        assert_eq!(raw[8..12], 1u32.to_le_bytes());
        assert_eq!(raw[12..16], 4096u32.to_le_bytes());
        assert_eq!(raw[16..20], 2u32.to_le_bytes());
        assert_eq!(raw[20..24], 37.5f32.to_bits().to_le_bytes());
        assert_eq!(raw[24..28], 0u32.to_le_bytes());
        assert!(raw[28..].iter().all(|&b| b == 0));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
