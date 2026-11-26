//! The desktop-look reader against a fake `org.freedesktop.portal.Settings`
//! on a PRIVATE session bus: first read, `SettingChanged`, a portal that
//! starts late, restarts on the deprecated `Read`, and KDE's answers (the
//! portal's and the `kdeglobals` file's). Self-skips without `dbus-daemon`.
//!
//! The bus has no service directories (`common::private_bus`), so nothing
//! is auto-started for the portal it starts without. Single #[test]: the bus address and the desktop variables are
//! process-global.

mod common;

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedValue, Value};

use hushmic::desktop::{self, Look, Scheme};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

type Values = Arc<Mutex<HashMap<(String, String), OwnedValue>>>;

struct FakeSettings {
    version: u32,
    values: Values,
}

impl FakeSettings {
    fn value(&self, ns: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
        self.values
            .lock()
            .unwrap()
            .get(&(ns.to_string(), key.to_string()))
            .map(|v| v.try_clone().unwrap())
            .ok_or_else(|| zbus::fdo::Error::Failed(format!("{ns} {key}: not found")))
    }
}

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl FakeSettings {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        self.version
    }

    /// Deprecated: the value in one more variant.
    fn read(&self, namespace: String, key: String) -> zbus::fdo::Result<OwnedValue> {
        let v = self.value(&namespace, &key)?;
        Ok(OwnedValue::try_from(Value::Value(Box::new(Value::from(v)))).unwrap())
    }

    fn read_one(&self, namespace: String, key: String) -> zbus::fdo::Result<OwnedValue> {
        if self.version < 2 {
            return Err(zbus::fdo::Error::UnknownMethod("ReadOne".into()));
        }
        self.value(&namespace, &key)
    }
}

fn values(pairs: &[(&str, &str, Value<'static>)]) -> Values {
    Arc::new(Mutex::new(
        pairs
            .iter()
            .map(|(ns, k, v)| {
                (
                    (ns.to_string(), k.to_string()),
                    OwnedValue::try_from(v.try_clone().unwrap()).unwrap(),
                )
            })
            .collect(),
    ))
}

fn portal(
    rt: &tokio::runtime::Runtime,
    addr: &str,
    version: u32,
    values: Values,
) -> zbus::Connection {
    rt.block_on(async {
        zbus::connection::Builder::address(addr)
            .unwrap()
            .name(PORTAL)
            .unwrap()
            .serve_at(PATH, FakeSettings { version, values })
            .unwrap()
            .build()
            .await
            .expect("fake portal on the private bus")
    })
}

/// The portal's value changes and it says so.
fn changed(
    rt: &tokio::runtime::Runtime,
    conn: &zbus::Connection,
    values: &Values,
    ns: &str,
    key: &str,
    v: Value,
) {
    values.lock().unwrap().insert(
        (ns.to_string(), key.to_string()),
        OwnedValue::try_from(v.try_clone().unwrap()).unwrap(),
    );
    signal(rt, conn, ns, key, v);
}

/// A `SettingChanged` alone, whatever the portal answers when asked.
fn signal(rt: &tokio::runtime::Runtime, conn: &zbus::Connection, ns: &str, key: &str, v: Value) {
    rt.block_on(async {
        conn.emit_signal(
            Option::<&str>::None,
            PATH,
            "org.freedesktop.portal.Settings",
            "SettingChanged",
            &(ns, key, v),
        )
        .await
        .expect("emit SettingChanged")
    });
}

/// Reports until `want` arrives (a portal coming or going may report a
/// "nothing says" look on the way).
fn expect_look(rx: &mpsc::Receiver<Look>, want: Look) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(l) if l == want => return,
            Ok(l) => seen.push(l),
            Err(e) => panic!("no {want:?} within 10 s ({e}); saw {seen:?}"),
        }
    }
}

fn look(scheme: Option<Scheme>, reduced_motion: Option<bool>) -> Look {
    Look {
        scheme,
        reduced_motion,
    }
}

#[test]
fn desktop_look_follows_the_settings_portal() {
    let Some(bus) = common::private_bus("desktop_look_follows_the_settings_portal") else {
        return;
    };
    let addr = bus.addr.clone();
    let dir = bus.dir.clone();
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);
    // No kdeglobals of the user's may leak in.
    let config = dir.join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &config);
    std::env::set_var("XDG_CURRENT_DESKTOP", "GNOME");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();

    // --- no portal: nothing says (dark, full motion downstream) ---
    let (tx, rx) = mpsc::channel();
    let reader = desktop::spawn(
        move |l| {
            let _ = tx.send(l);
        },
        None,
    );
    // The first answer is there for the window opened at launch.
    assert_eq!(reader.first(Duration::from_secs(5)), Some(look(None, None)));
    expect_look(&rx, look(None, None));

    // --- the portal starts late: read at once ---
    let gnome = values(&[
        ("org.freedesktop.appearance", "color-scheme", Value::U32(1)),
        (
            "org.gnome.desktop.interface",
            "enable-animations",
            Value::Bool(false),
        ),
    ]);
    let p = portal(&rt, &addr, 2, gnome.clone());
    expect_look(&rx, look(Some(Scheme::Dark), Some(true)));

    // --- changes follow ---
    changed(
        &rt,
        &p,
        &gnome,
        "org.freedesktop.appearance",
        "color-scheme",
        Value::U32(2),
    );
    expect_look(&rx, look(Some(Scheme::Light), Some(true)));
    changed(
        &rt,
        &p,
        &gnome,
        "org.gnome.desktop.interface",
        "enable-animations",
        Value::Bool(true),
    );
    expect_look(&rx, look(Some(Scheme::Light), Some(false)));
    // The portal's own reduced-motion wins over GNOME's key.
    changed(
        &rt,
        &p,
        &gnome,
        "org.freedesktop.appearance",
        "reduced-motion",
        Value::U32(1),
    );
    expect_look(&rx, look(Some(Scheme::Light), Some(true)));
    // Someone else's setting changes nothing.
    changed(
        &rt,
        &p,
        &gnome,
        "org.gnome.desktop.interface",
        "font-name",
        Value::from("Sans 11"),
    );
    changed(
        &rt,
        &p,
        &gnome,
        "org.freedesktop.appearance",
        "color-scheme",
        Value::U32(0),
    );
    expect_look(&rx, look(Some(Scheme::NoPreference), Some(true)));
    // A signal that disagrees with the portal's own answer (Plasma 5.27
    // announcing a switch from a half-written kdeglobals): once changes
    // settle, what the portal answers stands.
    gnome.lock().unwrap().insert(
        (
            "org.freedesktop.appearance".to_string(),
            "color-scheme".to_string(),
        ),
        OwnedValue::from(2u32),
    );
    signal(
        &rt,
        &p,
        "org.freedesktop.appearance",
        "color-scheme",
        Value::U32(0),
    );
    expect_look(&rx, look(Some(Scheme::Light), Some(true)));

    // --- a restart on an old portal (Read only) on KDE, where a GTK
    // backend also answers GNOME's key: KDE's own setting wins ---
    rt.block_on(async { drop(p) });
    // Named by a launch (this process's environment still says GNOME, as a
    // daemon started before the session knows nothing).
    reader.desktop("KDE");
    let kde = values(&[
        ("org.freedesktop.appearance", "color-scheme", Value::U32(1)),
        (
            "org.gnome.desktop.interface",
            "enable-animations",
            Value::Bool(true),
        ),
        (
            "org.kde.kdeglobals.KDE",
            "AnimationDurationFactor",
            Value::from("0"),
        ),
    ]);
    let p = portal(&rt, &addr, 1, kde.clone());
    expect_look(&rx, look(Some(Scheme::Dark), Some(true)));
    changed(
        &rt,
        &p,
        &kde,
        "org.kde.kdeglobals.KDE",
        "AnimationDurationFactor",
        Value::from("1"),
    );
    expect_look(&rx, look(Some(Scheme::Dark), Some(false)));

    // --- KDE without the portal's kdeglobals: the file answers ---
    rt.block_on(async { drop(p) });
    std::fs::write(
        config.join("kdeglobals"),
        "[KDE]\nAnimationDurationFactor=0\n",
    )
    .unwrap();
    let plain = values(&[("org.freedesktop.appearance", "color-scheme", Value::U32(2))]);
    let _p = portal(&rt, &addr, 2, plain);
    expect_look(&rx, look(Some(Scheme::Light), Some(true)));
    // The file is not followed; a refresh (a window opening) reads it again.
    std::fs::write(
        config.join("kdeglobals"),
        "[KDE]\nAnimationDurationFactor=1\n",
    )
    .unwrap();
    reader.refresh();
    expect_look(&rx, look(Some(Scheme::Light), Some(false)));
}
