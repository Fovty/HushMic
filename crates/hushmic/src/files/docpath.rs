//! A folder the document portal handed over (a sandbox's chosen folder)
//! lives at `/run/user/UID/doc/ID/NAME` (`/run/flatpak/doc/ID/NAME` in a
//! Flatpak): its ID means nothing to the user. The portal's
//! `Documents.GetHostPaths` (version 5 on) says where it is on the host;
//! where it cannot, the folder is shown by its own name.
//!
//! The lookup runs on a thread of its own, bounded, once per ID: the
//! window shows the name meanwhile and repaints when the answer is in.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// How long the portal may take to answer.
#[cfg(not(test))]
const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The document ID of a path in the portal's mount, and the rest of the
/// path after it (the exported entry first). None: not a portal path.
pub fn doc_id(path: &Path) -> Option<(String, PathBuf)> {
    if !path.is_absolute() {
        return None;
    }
    // after the root, names only (no "." or "..")
    let names: Vec<&std::ffi::OsStr> = path
        .components()
        .skip(1)
        .map(|c| match c {
            Component::Normal(n) => Some(n),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let at = match names.as_slice() {
        [run, user, uid, doc, ..]
            if *run == "run"
                && *user == "user"
                && *doc == "doc"
                && uid
                    .to_str()
                    .is_some_and(|u| u.bytes().all(|b| b.is_ascii_digit())) =>
        {
            4
        }
        [run, flatpak, doc, ..] if *run == "run" && *flatpak == "flatpak" && *doc == "doc" => 3,
        _ => return None,
    };
    let id = names.get(at)?.to_str()?.to_string();
    let rest: PathBuf = names[at + 1..].iter().collect();
    Some((id, rest))
}

/// Where a portal path is on the host, if the portal said so already;
/// asks it (once per ID) otherwise. None: not a portal path, or no answer
/// (yet).
pub fn host_path(path: &Path) -> Option<PathBuf> {
    let (id, rest) = doc_id(path)?;
    let host = {
        let mut cache = cache().lock().ok()?;
        match cache.get(&id) {
            Some(answer) => answer.clone(),
            None => {
                cache.insert(id.clone(), None);
                ask(id);
                None
            }
        }
    }?;
    // the ID stands for the exported entry itself: the rest goes below it
    let mut below = rest.components();
    below.next();
    Some(host.join(below.as_path()))
}

/// What each window registers while it is up: a repaint, when an answer
/// is in. Every window the process opens gets one, until its guard goes.
#[must_use = "the window stops being woken when the guard is dropped"]
pub fn on_answer(wake: impl Fn() + Send + Sync + 'static) -> Woken {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut w) = wakes().lock() {
        w.push((id, Box::new(wake)));
    }
    Woken(id)
}

/// A window's place among those woken (see [`on_answer`]).
pub struct Woken(u64);

impl Drop for Woken {
    fn drop(&mut self) {
        if let Ok(mut w) = wakes().lock() {
            w.retain(|(id, _)| *id != self.0);
        }
    }
}

type Wake = Box<dyn Fn() + Send + Sync>;

fn wakes() -> &'static Mutex<Vec<(u64, Wake)>> {
    static WAKES: OnceLock<Mutex<Vec<(u64, Wake)>>> = OnceLock::new();
    WAKES.get_or_init(Mutex::default)
}

/// Repaint every window that is up.
fn wake_all() {
    if let Ok(w) = wakes().lock() {
        for (_, wake) in w.iter() {
            wake();
        }
    }
}

fn cache() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

#[cfg(test)]
fn ask(_id: String) {}

#[cfg(not(test))]
fn ask(id: String) {
    let _ = std::thread::Builder::new()
        .name("hushmic-docpath".into())
        .spawn(move || {
            let answer = lookup(&id);
            if let Ok(mut cache) = cache().lock() {
                cache.insert(id, answer.clone());
            }
            if answer.is_some() {
                wake_all();
            }
        });
}

#[cfg(not(test))]
fn lookup(id: &str) -> Option<PathBuf> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        tokio::time::timeout(LOOKUP_TIMEOUT, host_paths(id))
            .await
            .ok()
            .flatten()
    })
}

#[cfg(not(test))]
async fn host_paths(id: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let conn = zbus::Connection::session().await.ok()?;
    let docs = zbus::Proxy::new(
        &conn,
        "org.freedesktop.portal.Documents",
        "/org/freedesktop/portal/documents",
        "org.freedesktop.portal.Documents",
    )
    .await
    .ok()?;
    let paths: HashMap<String, Vec<u8>> = docs.call("GetHostPaths", &(vec![id],)).await.ok()?;
    let mut bytes = paths.get(id)?.clone();
    // a bytestring: NUL-terminated
    if bytes.last() == Some(&0) {
        bytes.pop();
    }
    (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Tests: the portal's answer for `id`.
#[cfg(test)]
pub fn answered(id: &str, host: Option<&str>) {
    cache()
        .lock()
        .unwrap()
        .insert(id.into(), host.map(PathBuf::from));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_paths_are_told_by_their_mount() {
        assert_eq!(
            doc_id(Path::new("/run/user/1000/doc/154e108f/Musik")),
            Some(("154e108f".into(), PathBuf::from("Musik")))
        );
        assert_eq!(
            doc_id(Path::new("/run/flatpak/doc/154e108f/Musik/Cleaned")),
            Some(("154e108f".into(), PathBuf::from("Musik/Cleaned")))
        );
        for not in [
            "/home/you/Music",
            "/run/user/you/doc/1/Musik",
            "run/user/1000/doc/1/Musik",
            "/run/user/1000/doc",
            "/run/user/1000/other/1/Musik",
        ] {
            assert_eq!(doc_id(Path::new(not)), None, "{not}");
        }
    }

    #[test]
    fn a_host_path_is_the_portals_answer_with_the_rest_below_it() {
        answered("a1", Some("/home/you/Musik"));
        assert_eq!(
            host_path(Path::new("/run/user/1000/doc/a1/Musik")),
            Some(PathBuf::from("/home/you/Musik"))
        );
        assert_eq!(
            host_path(Path::new("/run/flatpak/doc/a1/Musik/Cleaned")),
            Some(PathBuf::from("/home/you/Musik/Cleaned"))
        );
        // no answer (an older portal): none, and asked only once
        answered("b2", None);
        assert_eq!(host_path(Path::new("/run/user/1000/doc/b2/Musik")), None);
        assert_eq!(host_path(Path::new("/home/you/Musik")), None);
    }

    #[test]
    fn every_window_the_process_opens_is_woken_while_it_is_up() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let counter = |n: &Arc<AtomicUsize>| {
            let n = Arc::clone(n);
            move || {
                n.fetch_add(1, Ordering::Relaxed);
            }
        };
        let (first, second) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        // the first window, then a second one while it is still up
        let one = on_answer(counter(&first));
        let two = on_answer(counter(&second));
        wake_all();
        assert_eq!(
            (
                first.load(Ordering::Relaxed),
                second.load(Ordering::Relaxed)
            ),
            (1, 1)
        );
        // the first one closed: only the second is woken
        drop(one);
        wake_all();
        assert_eq!(
            (
                first.load(Ordering::Relaxed),
                second.load(Ordering::Relaxed)
            ),
            (1, 2)
        );
        drop(two);
        wake_all();
        assert_eq!(second.load(Ordering::Relaxed), 2);
    }
}
