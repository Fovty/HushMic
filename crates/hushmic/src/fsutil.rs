//! Crash-safe small-file writes.

use std::io::Write;
use std::path::Path;

/// Write `contents` to `path` such that a crash can never leave a truncated
/// file: write a sibling temp file, fsync it, then rename over the target.
///
/// The classic zero-length-file-after-power-cut happens because plain
/// `fs::write` is open(TRUNC)+write — the truncation hits the journal while
/// the data is still in the page cache (observed live: a hard VM reset right
/// after toggling "Start on login" left a 0-byte hushmic.desktop, so nothing
/// autostarted). The fsync before the rename matters: without it the rename
/// can become durable before the data, reintroducing the empty-file window.
///
/// Deliberately no fsync on the parent directory: if the rename itself is
/// lost to a crash, the previous file survives untouched — stale-but-intact
/// beats empty.
pub fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d)?;
    }
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .ok_or_else(|| std::io::Error::other("atomic_write: path has no file name"))?;
    name.push(".tmp");
    let tmp = path.with_file_name(&name);
    let write = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_overwrites_without_leftovers() {
        let dir = std::env::temp_dir().join(format!("hushmic-fsutil-{}", std::process::id()));
        let p = dir.join("sub").join("file.desktop");
        atomic_write(&p, b"first").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"first");
        atomic_write(&p, b"second, longer content").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"second, longer content");
        // No temp file may survive a successful write.
        let names: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("file.desktop")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_pathless_target() {
        assert!(atomic_write(Path::new("/"), b"x").is_err());
    }
}
