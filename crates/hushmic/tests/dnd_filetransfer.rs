//! A drop into the Flatpak: the FileTransfer portal's key traded for the
//! paths the sandbox can open, against a fake portal on a PRIVATE session
//! bus (a key it knows, one it does not, and an empty one; and the same
//! bytes as a dropped offer carries them). A portal that never answers:
//! the drop gives up at its cap, and the clipboard's device (whose thread
//! paste waits on too) goes on meanwhile. Self-skips without
//! `dbus-daemon`. Single #[test]: the bus address is process-global.

mod common;

use eframe::egui;
use hushmic::window::dnd::{
    paths, retrieve_files, Drops, Source, PORTAL_CALL_CAP, PORTAL_KEY, URI_LIST,
};
use smithay_clipboard::dnd::{testing as device, Event};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::zvariant::OwnedValue;

#[derive(Clone, Default)]
struct FakeTransfer {
    /// key -> the files the portal made reachable for it
    keys: Arc<Mutex<HashMap<String, Vec<String>>>>,
    asked: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.freedesktop.portal.FileTransfer")]
impl FakeTransfer {
    async fn retrieve_files(
        &self,
        key: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<Vec<String>> {
        self.asked.lock().unwrap().push(key.clone());
        if key == "stall" {
            std::future::pending::<()>().await;
        }
        self.keys
            .lock()
            .unwrap()
            .remove(&key)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Invalid transfer".into()))
    }
}

#[test]
fn a_portal_key_becomes_the_sandboxes_paths() {
    let Some(bus) = common::private_bus("dnd-filetransfer") else {
        return;
    };
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.addr);
    let fake = FakeTransfer::default();
    let doc = "/run/user/1000/doc/3f2a91c0";
    fake.keys.lock().unwrap().extend([
        (
            "1234".to_string(),
            vec![format!("{doc}/My talk.m4a"), format!("{doc}/b.wav")],
        ),
        ("5678".to_string(), vec![format!("{doc}/c.flac")]),
    ]);
    let _portal = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.portal.Documents")
        .unwrap()
        .serve_at("/org/freedesktop/portal/documents", fake.clone())
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        retrieve_files("1234"),
        Ok(vec![
            PathBuf::from(format!("{doc}/My talk.m4a")),
            PathBuf::from(format!("{doc}/b.wav")),
        ])
    );
    // A key is good once (the portal forgets it), and an unknown one fails.
    assert!(retrieve_files("1234").is_err());
    assert!(retrieve_files("nope").is_err());
    // The offer's bytes as GTK sends them: the key, NUL-terminated.
    assert_eq!(
        paths(Source::Portal, b"5678\0"),
        Ok(vec![PathBuf::from(format!("{doc}/c.flac"))])
    );
    // Nothing to ask with: no call at all.
    assert!(retrieve_files("\0").is_err());
    assert_eq!(
        *fake.asked.lock().unwrap(),
        ["1234", "1234", "nope", "5678"]
    );

    // The portal stalls on a drop. The drop's own thread waits for it, up
    // to the cap; the device's thread picks types and passes the next
    // drag's events meanwhile (it never waits on a drop's handler).
    let drops = Drops::start(&egui::Context::default());
    let dropped = Arc::new(AtomicBool::new(false));
    let t = Instant::now();
    let dropping = {
        let dropped = dropped.clone();
        std::thread::spawn(move || {
            device::emit(Event::Drop {
                mime: PORTAL_KEY.into(),
                data: Ok(b"stall\0".to_vec()),
            });
            dropped.store(true, Ordering::SeqCst);
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !fake.asked.lock().unwrap().iter().any(|k| k == "stall") {
        assert!(Instant::now() < deadline, "the drop never asked the portal");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (tx, rx) = mpsc::channel();
    {
        let dropped = dropped.clone();
        std::thread::spawn(move || {
            let picked = device::pick(&[URI_LIST.into()]);
            device::emit(Event::Enter {
                mimes: vec![URI_LIST.into()],
                accepted: true,
            });
            device::emit(Event::Leave);
            let _ = tx.send((picked, dropped.load(Ordering::SeqCst)));
        });
    }
    let (picked, after_drop) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the device's thread is stuck");
    assert_eq!(picked.as_deref(), Some(URI_LIST));
    assert!(
        !after_drop,
        "the device's thread waited for the stalled drop"
    );
    dropping.join().unwrap();
    let took = t.elapsed();
    assert!(took >= PORTAL_CALL_CAP, "gave up after {took:?}");
    assert!(
        took < PORTAL_CALL_CAP + Duration::from_secs(30),
        "took {took:?}"
    );
    let mut raw = egui::RawInput::default();
    drops.feed(&mut raw);
    assert!(raw.dropped_files.is_empty() && raw.hovered_files.is_empty());
}
