//! Drops on Wayland. winit 0.30 takes no drops there, so they come
//! through egui's clipboard device instead: the workspace patches
//! smithay-clipboard (vendor/smithay-clipboard) to take drags on the data
//! device it keeps for copy and paste, the one device every compositor
//! serves (KWin gives drags to a client's first device only, Mutter the
//! selection to its newest). What it hands over is fed into egui's input as
//! hovered and dropped files, so the pages take Wayland drops the way they
//! take X11's.
//!
//! A drop offers its files as `text/uri-list`; inside a Flatpak those
//! paths are the host's and out of reach, so there the FileTransfer
//! portal's key (`application/vnd.portal.filetransfer`) is taken first and
//! traded for paths the sandbox can open. The trade happens before the
//! drop is finished: the key stops working after that.

use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const URI_LIST: &str = "text/uri-list";
pub const PORTAL_KEY: &str = "application/vnd.portal.filetransfer";

/// How a drop's files are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `text/uri-list`: the paths themselves.
    UriList,
    /// The FileTransfer portal's key, traded for paths (in a Flatpak).
    Portal,
}

impl Source {
    pub fn mime(self) -> &'static str {
        match self {
            Source::UriList => URI_LIST,
            Source::Portal => PORTAL_KEY,
        }
    }
}

/// The MIME types a drop is taken in, by preference.
pub fn accept(flatpak: bool) -> Vec<String> {
    let mut m = Vec::new();
    if flatpak {
        m.push(PORTAL_KEY.to_string());
    }
    m.push(URI_LIST.to_string());
    m
}

impl Source {
    pub fn of(mime: &str) -> Option<Source> {
        match mime {
            URI_LIST => Some(Source::UriList),
            PORTAL_KEY => Some(Source::Portal),
            _ => None,
        }
    }
}

/// The local files a `text/uri-list` names (RFC 2483: one URI a line, CRLF,
/// `#` comments); anything but `file://` on this host is left out.
pub fn uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c == '\r' || c == '\0' || c == ' '))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(crate::files::chooser::uri_path)
        .collect()
}

/// How long a drop waits for the FileTransfer portal (the bus has no reply
/// timeout of its own; a drop that waits longer comes to nothing).
pub const PORTAL_CALL_CAP: std::time::Duration = std::time::Duration::from_secs(5);

/// The paths a FileTransfer key stands for, through the portal on the
/// session bus (blocking, up to [`PORTAL_CALL_CAP`]).
pub fn retrieve_files(key: &str) -> Result<Vec<PathBuf>, String> {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let key = key.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    if key.is_empty() {
        return Err("an empty FileTransfer key".into());
    }
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.method_timeout(PORTAL_CALL_CAP).build())
        .map_err(|e| format!("no session bus ({e})"))?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Documents",
        "/org/freedesktop/portal/documents",
        "org.freedesktop.portal.FileTransfer",
    )
    .map_err(|e| e.to_string())?;
    let options: HashMap<&str, Value> = HashMap::new();
    let files: Vec<String> = proxy
        .call("RetrieveFiles", &(key, options))
        .map_err(|e| format!("FileTransfer: {e}"))?;
    Ok(files.into_iter().map(PathBuf::from).collect())
}

/// The paths a dropped offer's bytes (read as `source`) stand for.
pub fn paths(source: Source, bytes: &[u8]) -> Result<Vec<PathBuf>, String> {
    let text = String::from_utf8_lossy(bytes);
    match source {
        Source::UriList => Ok(uri_list(&text)),
        Source::Portal => retrieve_files(&text),
    }
}

/// What the clipboard's device saw, for the window's frames.
#[derive(Default)]
struct Seen {
    /// A drag with files is over the window.
    hovering: bool,
    /// Dropped files not yet given to egui.
    dropped: Vec<PathBuf>,
}

/// The window's Wayland drops, fed into each frame's input.
pub struct Drops {
    seen: Arc<Mutex<Seen>>,
}

impl Drops {
    /// Take drops through the clipboard's device. One window per process:
    /// the handler is the process's. On X11 it is never called (egui-winit
    /// takes drops there).
    pub fn start(ctx: &egui::Context) -> Drops {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (s, ctx) = (seen.clone(), ctx.clone());
        smithay_clipboard::dnd::set_handler(accept(crate::sandbox::is_flatpak()), move |e| {
            take(&s, e);
            ctx.request_repaint();
        });
        Drops { seen }
    }

    /// Put what the device saw into this frame's input.
    pub fn feed(&self, raw: &mut egui::RawInput) {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.hovering {
            raw.hovered_files.push(egui::HoveredFile {
                path: None,
                mime: URI_LIST.into(),
            });
        }
        raw.dropped_files
            .extend(seen.dropped.drain(..).map(|path| egui::DroppedFile {
                path: Some(path),
                ..Default::default()
            }));
    }
}

/// One event from the device (on its thread; a drop's paths are looked up
/// here, before the device finishes the drop).
fn take(seen: &Mutex<Seen>, event: smithay_clipboard::dnd::Event) {
    use smithay_clipboard::dnd::Event;
    let got = match event {
        Event::Enter { accepted, .. } => {
            lock(seen).hovering = accepted;
            return;
        }
        // (on GNOME the leave comes between the drop and its data)
        Event::Leave => {
            lock(seen).hovering = false;
            return;
        }
        Event::Drop { mime, data } => match (Source::of(&mime), data) {
            (Some(source), Ok(bytes)) => paths(source, &bytes),
            (None, _) => Err(format!("a drop as {mime}")),
            (_, Err(e)) => Err(format!("reading the drop: {e}")),
        },
    };
    let mut s = lock(seen);
    s.hovering = false;
    match got {
        Ok(paths) => s.dropped.extend(paths),
        Err(e) => eprintln!("[hushmic] window: a drop came to nothing ({e})"),
    }
}

fn lock(seen: &Mutex<Seen>) -> std::sync::MutexGuard<'_, Seen> {
    seen.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_uri_list_names_local_files_only() {
        let text = "# from a file manager\r\n\
                    file:///home/a/My%20talk.m4a\r\n\
                    file://localhost/tmp/b.wav\r\n\
                    \r\n\
                    https://example.com/c.mp3\r\n\
                    file://elsewhere/d.wav\r\n\
                    file:///tmp/%C3%A4%20%2350%25.flac";
        assert_eq!(
            uri_list(text),
            [
                Path::new("/home/a/My talk.m4a"),
                Path::new("/tmp/b.wav"),
                Path::new("/tmp/ä #50%.flac"),
            ]
        );
        // LF only, no trailing newline, a NUL some toolkits end with
        assert_eq!(
            uri_list("file:///a\nfile:///b\0"),
            [Path::new("/a"), Path::new("/b")]
        );
        assert!(uri_list("").is_empty());
        assert!(uri_list("plain text\nnot a uri").is_empty());
    }

    #[test]
    fn a_non_utf8_name_comes_through() {
        use std::os::unix::ffi::OsStrExt;
        let got = uri_list("file:///tmp/%FF.wav");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_os_str().as_bytes(), b"/tmp/\xff.wav");
    }

    #[test]
    fn files_are_taken_as_the_sandbox_can_open_them() {
        assert_eq!(accept(false), [URI_LIST]);
        assert_eq!(accept(true), [PORTAL_KEY, URI_LIST]);
        assert_eq!(Source::of(URI_LIST), Some(Source::UriList));
        assert_eq!(Source::of(PORTAL_KEY), Some(Source::Portal));
        assert_eq!(Source::of("text/plain"), None);
    }

    #[test]
    fn a_drag_hovers_and_its_drop_lands_once_left() {
        use smithay_clipboard::dnd::Event;
        let seen = Mutex::new(Seen::default());
        let enter = |accepted| Event::Enter {
            mimes: vec![URI_LIST.into()],
            accepted,
        };
        take(&seen, enter(false));
        assert!(!seen.lock().unwrap().hovering, "nothing a page can take");
        take(&seen, enter(true));
        assert!(seen.lock().unwrap().hovering);
        // GNOME: the leave comes before the drop's data; the drop still lands.
        take(&seen, Event::Leave);
        assert!(!seen.lock().unwrap().hovering);
        take(
            &seen,
            Event::Drop {
                mime: URI_LIST.into(),
                data: Ok(b"file:///tmp/a%20b.wav\r\nfile:///tmp/c.flac\r\n".to_vec()),
            },
        );
        assert_eq!(
            seen.lock().unwrap().dropped,
            [PathBuf::from("/tmp/a b.wav"), PathBuf::from("/tmp/c.flac")]
        );
        // A drop that could not be read, or in a type not asked for: nothing.
        seen.lock().unwrap().dropped.clear();
        take(&seen, enter(true));
        take(
            &seen,
            Event::Drop {
                mime: URI_LIST.into(),
                data: Err(std::io::Error::other("pipe")),
            },
        );
        take(
            &seen,
            Event::Drop {
                mime: "text/plain".into(),
                data: Ok(b"file:///tmp/x.wav".to_vec()),
            },
        );
        let s = seen.lock().unwrap();
        assert!(s.dropped.is_empty() && !s.hovering);
    }

    #[test]
    fn hover_and_drops_reach_the_frame_once() {
        let drops = Drops {
            seen: Arc::new(Mutex::new(Seen::default())),
        };
        let mut raw = egui::RawInput::default();
        drops.feed(&mut raw);
        assert!(raw.hovered_files.is_empty() && raw.dropped_files.is_empty());
        drops.seen.lock().unwrap().hovering = true;
        let mut raw = egui::RawInput::default();
        drops.feed(&mut raw);
        assert_eq!(raw.hovered_files.len(), 1);
        {
            let mut s = drops.seen.lock().unwrap();
            s.hovering = false;
            s.dropped = vec!["/a.wav".into(), "/b.wav".into()];
        }
        let mut raw = egui::RawInput::default();
        drops.feed(&mut raw);
        assert!(raw.hovered_files.is_empty());
        let got: Vec<_> = raw
            .dropped_files
            .iter()
            .filter_map(|f| f.path.clone())
            .collect();
        assert_eq!(got, [PathBuf::from("/a.wav"), PathBuf::from("/b.wav")]);
        let mut raw = egui::RawInput::default();
        drops.feed(&mut raw);
        assert!(raw.dropped_files.is_empty(), "given once");
    }
}
