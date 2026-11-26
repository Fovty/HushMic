//! The FileChooser portal for the Files page: recordings to clean (one or
//! several) and a folder (where cleaned files go). Through the portal the
//! picks work the same inside a Flatpak, where they come with the
//! sandbox's access to them.
//!
//! The same request pattern as the other portals (`shortcuts::
//! portal_request`: the handle token, the Response signal subscribed first,
//! a bounded wait). Blocking: call it off the window's frame.

use crate::shortcuts::{portal_request, PORTAL_NAME};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use zbus::zvariant::{OwnedValue, Value};

/// What to pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Recordings: audio and video files, several at once.
    Recordings,
    /// One recording ("Choose it again…").
    Recording,
    /// One folder.
    Folder,
}

/// A dialog the user leaves open is waited for this long, then given up.
pub const WAIT: Duration = Duration::from_secs(3600);

/// Ask with `title` over the window `parent` (its portal identifier, "x11:…"
/// or "wayland:…", or "" for none). The picked paths; None when the user
/// cancelled.
pub fn choose(
    pick: Pick,
    title: &str,
    parent: &str,
    wait: Duration,
) -> Result<Option<Vec<PathBuf>>, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(choose_inner(pick, title, parent, wait))
}

async fn choose_inner(
    pick: Pick,
    title: &str,
    parent: &str,
    wait: Duration,
) -> Result<Option<Vec<PathBuf>>, String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("no session bus ({e})"))?;
    let portal = zbus::Proxy::new(
        &conn,
        PORTAL_NAME,
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.FileChooser",
    )
    .await
    .map_err(|e| e.to_string())?;
    let (code, results) = portal_request(&conn, &portal, "OpenFile", wait, |token| {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("handle_token", token.to_string().into());
        options.insert("modal", true.into());
        match pick {
            Pick::Recordings | Pick::Recording => {
                options.insert("multiple", (pick == Pick::Recordings).into());
                let filters = recording_filters(by_type_keeps_names());
                options.insert("current_filter", filters[0].clone().into());
                options.insert("filters", filters.into());
            }
            Pick::Folder => {
                options.insert("directory", true.into());
            }
        }
        (parent.to_string(), title.to_string(), options)
    })
    .await?;
    match code {
        0 => Ok(Some(paths(&results))),
        1 => Ok(None),
        c => Err(format!("the file chooser failed ({c})")),
    }
}

/// A chooser filter: its name, and its patterns (0: a glob, 1: a MIME
/// type).
type Filter = (String, Vec<(u32, String)>);

/// Recordings, then All files. Recordings go by glob and (`by_type`) by
/// concrete MIME type: Plasma 5.27's portal matches no `audio/*` wildcard
/// (its folders showed empty), a file whose type the desktop doesn't know
/// still matches its extension, and one without an extension its type.
/// All files lets anything else be tried.
fn recording_filters(by_type: bool) -> Vec<Filter> {
    use super::kinds;
    let mut rec: Vec<(u32, String)> = kinds::globs().into_iter().map(|g| (0, g)).collect();
    if by_type {
        rec.extend(kinds::MIME.iter().map(|m| (1, m.to_string())));
    }
    vec![
        (crate::tr!("files-chooser-recordings"), rec),
        (crate::tr!("files-chooser-all"), vec![(0, "*".to_string())]),
    ]
}

/// Whether the desktop's chooser keeps the filters' names when they hold
/// MIME types. Plasma 5's portal does not: given any MIME type it lists
/// each type on its own (forty of them) and drops every glob filter, All
/// files too (seen on Plasma 5.27). Its globs alone match well enough
/// (by extension, any case); only files without one need All files there.
fn by_type_keeps_names() -> bool {
    !plasma_5(
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        std::env::var("KDE_SESSION_VERSION").ok().as_deref(),
    )
}

fn plasma_5(desktop: Option<&str>, kde: Option<&str>) -> bool {
    let kde_desktop = desktop.is_some_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("KDE")));
    kde_desktop && kde.is_some_and(|v| v.trim() == "5" || v.trim() == "4")
}

/// The `uris` of a response as paths (file URIs only).
fn paths(results: &HashMap<String, OwnedValue>) -> Vec<PathBuf> {
    let Some(v) = results.get("uris") else {
        return Vec::new();
    };
    let uris: Vec<String> = v
        .try_clone()
        .ok()
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default();
    uris.iter().filter_map(|u| uri_path(u)).collect()
}

/// A `file://` URI's path, percent-decoded.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    // "file://host/path": only the local host.
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(i) if &rest[..i] == "localhost" => &rest[i..],
        _ => return None,
    };
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::reveal::file_uri;
    use std::path::Path;

    #[test]
    fn recordings_go_by_glob_and_concrete_type_and_all_files_by_anything() {
        crate::i18n::pin_english();
        let f = recording_filters(true);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].0, "Recordings");
        assert!(f[0].1.contains(&(0, "*.mp4".into())));
        assert!(f[0].1.contains(&(0, "*.WAV".into())));
        assert!(f[0].1.contains(&(1, "audio/flac".into())));
        assert!(
            !f[0].1.iter().any(|(_, p)| p.ends_with("/*")),
            "no wildcard type"
        );
        assert_eq!(f[1], ("All files".into(), vec![(0, "*".into())]));
        // the portal's signature: a(sa(us)), and the current one (sa(us))
        let v: Value = f.clone().into();
        assert_eq!(v.value_signature().to_string(), "a(sa(us))");
        let c: Value = f[0].clone().into();
        assert_eq!(c.value_signature().to_string(), "(sa(us))");
        // Plasma 5: globs alone, so the names and All files stay
        let f = recording_filters(false);
        assert!(f[0].1.iter().all(|(k, _)| *k == 0));
        assert_eq!(f[1].0, "All files");
        assert!(plasma_5(Some("KDE"), Some("5")));
        assert!(!plasma_5(Some("KDE"), Some("6")));
        assert!(!plasma_5(Some("GNOME"), None));
        assert!(!plasma_5(Some("ubuntu:GNOME"), Some("5")));
    }

    #[test]
    fn uris_become_paths_and_back() {
        for p in ["/home/a/My Talk (1).wav", "/tmp/ä #2.flac", "/x/100%.mp3"] {
            assert_eq!(
                uri_path(&file_uri(Path::new(p))).as_deref(),
                Some(Path::new(p))
            );
        }
        assert_eq!(
            uri_path("file://localhost/tmp/a%20b").as_deref(),
            Some(Path::new("/tmp/a b"))
        );
        assert_eq!(uri_path("file:///tmp/50%"), Some(PathBuf::from("/tmp/50%")));
        assert_eq!(uri_path("https://x/y"), None);
        assert_eq!(uri_path("file://elsewhere/y"), None);
    }
}
