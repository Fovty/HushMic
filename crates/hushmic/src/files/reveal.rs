//! "Show in folder": the file manager opens the folder with the file
//! selected (`org.freedesktop.FileManager1.ShowItems`), else, in a sandbox
//! or without such a file manager, the OpenURI portal opens its folder.

use std::path::Path;
use std::time::Duration;

/// A file manager that does not answer must not hang the caller long.
const TIMEOUT: Duration = Duration::from_secs(5);

/// `path` as a `file://` URI, every byte outside the unreserved set and
/// `/` percent-encoded.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Show `path` in its folder. Blocking (a D-Bus call or two): call it off
/// the window's frame.
pub fn show_in_folder(path: &Path) -> Result<(), String> {
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.method_timeout(TIMEOUT).build())
        .map_err(|e| format!("no session bus ({e})"))?;
    let shown = conn.call_method(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        Some("org.freedesktop.FileManager1"),
        "ShowItems",
        &(vec![file_uri(path)], ""),
    );
    if shown.is_ok() {
        return Ok(());
    }
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = zbus::zvariant::Fd::from(&file);
    let options: std::collections::HashMap<&str, zbus::zvariant::Value> = Default::default();
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.OpenURI"),
        "OpenDirectory",
        &("", fd, options),
    )
    .map(|_| ())
    .map_err(|e| format!("neither a file manager nor the portal: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_encode_what_is_not_plain() {
        assert_eq!(
            file_uri(Path::new("/home/a/My Talk (cleaned) #1.wav")),
            "file:///home/a/My%20Talk%20%28cleaned%29%20%231.wav"
        );
        assert_eq!(
            file_uri(Path::new("/tmp/ä.flac")),
            "file:///tmp/%C3%A4.flac"
        );
    }
}
