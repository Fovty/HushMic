//! "Show in folder" on a PRIVATE session bus: the file manager's
//! `ShowItems` with the file's URI; without a file manager, the OpenURI
//! portal's `OpenDirectory` with the file itself. Self-skips without
//! `dbus-daemon`. Its own binary: the bus address is process-global.

use hushmic::files::reveal::{file_uri, show_in_folder};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

struct BusGuard(Child);
impl Drop for BusGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone, Default)]
struct FileManager(Arc<Mutex<Vec<Vec<String>>>>);

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    fn show_items(&self, uris: Vec<String>, _startup_id: String) {
        self.0.lock().unwrap().push(uris);
    }
}

#[derive(Clone, Default)]
struct OpenUri(Arc<Mutex<Vec<u64>>>);

#[zbus::interface(name = "org.freedesktop.portal.OpenURI")]
impl OpenUri {
    fn open_directory(
        &self,
        _parent: String,
        fd: zbus::zvariant::OwnedFd,
        _options: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    ) -> zbus::zvariant::OwnedObjectPath {
        // What the portal is handed is the file itself: its size says so.
        let file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
        self.0.lock().unwrap().push(file.metadata().unwrap().len());
        zbus::zvariant::OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1/t")
            .unwrap()
    }
}

#[test]
fn the_file_manager_shows_the_file_else_the_portal_its_folder() {
    if Command::new("dbus-daemon")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: dbus-daemon not available");
        return;
    }
    let mut daemon = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dbus-daemon");
    let addr = {
        let mut line = String::new();
        let _ = BufReader::new(daemon.stdout.take().unwrap()).read_line(&mut line);
        line.trim().to_string()
    };
    let _guard = BusGuard(daemon);
    if !addr.starts_with("unix:") {
        eprintln!("SKIP: no usable session bus address (got {addr:?})");
        return;
    }
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);
    let dir = std::env::temp_dir().join(format!("hushmic-reveal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("My talk (cleaned).wav");
    std::fs::write(&file, b"12345").unwrap();

    // No file manager on the bus: the portal.
    let opened = OpenUri::default();
    let portal = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at("/org/freedesktop/portal/desktop", opened.clone())
        .unwrap()
        .build()
        .unwrap();
    show_in_folder(&file).unwrap();
    assert_eq!(*opened.0.lock().unwrap(), [5]);

    // A file manager: it shows the file.
    let shown = FileManager::default();
    let _fm = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.FileManager1")
        .unwrap()
        .serve_at("/org/freedesktop/FileManager1", shown.clone())
        .unwrap()
        .build()
        .unwrap();
    show_in_folder(&file).unwrap();
    assert_eq!(*shown.0.lock().unwrap(), [vec![file_uri(&file)]]);
    assert_eq!(opened.0.lock().unwrap().len(), 1, "the portal is not asked");

    // Neither: it says so.
    drop((portal, _fm));
    assert!(show_in_folder(&file).is_err());
    let _ = std::fs::remove_dir_all(dir);
}
