//! The Files page's file chooser against a fake FileChooser portal on a
//! PRIVATE session bus: recordings (several, filtered to audio and video),
//! a folder, a cancel, and a portal that never answers. Self-skips without
//! `dbus-daemon`. Single #[test]: the bus address is process-global.

mod common;

use hushmic::files::chooser::{choose, Pick};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

/// What the dialog was asked: title, multiple, directory, a filter given.
type Asked = (String, bool, bool, bool);

/// An answer: the response code and the URIs.
type Answer = (u32, Vec<String>);

#[derive(Clone, Default)]
struct FakeChooser {
    /// The answers, in turn: the response code and the URIs; none left =
    /// the dialog stays open.
    answers: Arc<Mutex<Vec<Answer>>>,
    asked: Arc<Mutex<Vec<Asked>>>,
}

fn flag(options: &HashMap<String, OwnedValue>, key: &str) -> bool {
    options
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

#[zbus::interface(name = "org.freedesktop.portal.FileChooser")]
impl FakeChooser {
    async fn open_file(
        &self,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> OwnedObjectPath {
        let sender = hdr
            .sender()
            .unwrap()
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let token = options
            .get("handle_token")
            .and_then(|v| String::try_from(v.try_clone().unwrap()).ok())
            .unwrap();
        let request = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        self.asked.lock().unwrap().push((
            title,
            flag(&options, "multiple"),
            flag(&options, "directory"),
            options.contains_key("filters"),
        ));
        let answer = {
            let mut a = self.answers.lock().unwrap();
            (!a.is_empty()).then(|| a.remove(0))
        };
        if let Some((code, uris)) = answer {
            let mut results: HashMap<&str, Value> = HashMap::new();
            results.insert("uris", uris.into());
            conn.emit_signal(
                Option::<&str>::None,
                request.as_str(),
                "org.freedesktop.portal.Request",
                "Response",
                &(code, results),
            )
            .await
            .unwrap();
        }
        ObjectPath::try_from(request).unwrap().into()
    }
}

#[test]
fn recordings_and_a_folder_are_picked_and_a_cancel_is_none() {
    let Some(bus) = common::private_bus("files-chooser") else {
        return;
    };
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.addr);
    let fake = FakeChooser::default();
    *fake.answers.lock().unwrap() = vec![
        (
            0,
            vec![
                "file:///home/a/My%20talk.m4a".into(),
                "file:///home/a/b.wav".into(),
            ],
        ),
        (0, vec!["file:///home/a/Cleaned".into()]),
        (1, vec![]),
        (2, vec![]),
    ];
    let _portal = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at("/org/freedesktop/portal/desktop", fake.clone())
        .unwrap()
        .build()
        .unwrap();
    let wait = Duration::from_secs(10);
    assert_eq!(
        choose(Pick::Recordings, "Choose recordings", "", wait),
        Ok(Some(vec![
            PathBuf::from("/home/a/My talk.m4a"),
            PathBuf::from("/home/a/b.wav")
        ]))
    );
    assert_eq!(
        choose(Pick::Folder, "Choose where cleaned files go", "", wait),
        Ok(Some(vec![PathBuf::from("/home/a/Cleaned")]))
    );
    assert_eq!(choose(Pick::Folder, "x", "", wait), Ok(None), "cancelled");
    assert!(choose(Pick::Folder, "x", "", wait).is_err(), "failed");
    // A dialog left open: given up after the wait.
    assert!(choose(Pick::Folder, "x", "", Duration::from_millis(300)).is_err());
    let asked = fake.asked.lock().unwrap();
    assert_eq!(
        asked[0],
        ("Choose recordings".to_string(), true, false, true)
    );
    assert_eq!(
        asked[1],
        (
            "Choose where cleaned files go".to_string(),
            false,
            true,
            false
        )
    );
}
