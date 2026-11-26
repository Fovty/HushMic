//! Taking the system default first records the user's own default, so
//! that quitting hands it back. A read that gets no answer in time is not
//! "no default": nothing is recorded or written then, and a later try does
//! the takeover properly. (Treated as unset, the restore would delete the
//! user's configured default.) Turning the setting off hands it back while
//! the source host stays. Stand-in PipeWire tools keep the default in
//! a file; its own test binary.

use hushmic::config::Config;
use hushmic::controller::{Controller, Paths};
use hushmic::takeover::Caps;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn default_source(store: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(store).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(v.get("name")?.as_str()?.to_string())
}

/// How long the test waits for an attempt to come back. An attempt has no
/// fixed length: its caps bound the PipeWire calls, not starting them, nor
/// the write of the crash breadcrumb between them, which waits for the
/// disk. So this is no statement about an attempt, only wide: a failure
/// alone waits that long.
const PATIENCE: Duration = Duration::from_secs(120);

/// The attempt that is out, waited for and applied.
fn finish(c: &mut Controller) {
    assert!(c.finish_takeover(PATIENCE), "the attempt did not come back");
}

/// The state dir, where an attempt writes the crash breadcrumb and waits
/// for it to be on the disk: in memory when the box has /dev/shm, so that
/// wait is never for a disk other jobs are writing to (it counts against
/// the attempt's cap, and a busy disk can hold it for longer than that).
fn state_dir(root: &Path) -> PathBuf {
    let shm = Path::new("/dev/shm").join(root.file_name().unwrap());
    let _ = std::fs::remove_dir_all(&shm);
    if std::fs::create_dir_all(&shm).is_ok() {
        shm
    } else {
        root.join("state")
    }
}

#[test]
fn an_unanswered_read_takes_nothing_and_a_later_try_restores_the_users_default() {
    let root = std::env::temp_dir().join(format!("hm-takeover-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bin = root.join("bin");
    for d in ["bin", "home", "config", "state", "run", "models"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let state = state_dir(&root);
    script(
        &bin,
        "pipewire",
        r#"[ "$1" = --version ] && { echo "Compiled with libpipewire 1.0.5"; exit 0; }
exec sleep 60"#,
    );
    script(&bin, "pw-dump", "echo '[]'");
    // The default lives in `store`; while `hang` exists a read never
    // answers (the session manager busy), a write still lands.
    let store = root.join("default.json");
    let hang = root.join("hang");
    std::fs::write(&store, r#"{"name":"real_mic"}"#).unwrap();
    script(
        &bin,
        "pw-metadata",
        &format!(
            r#"store={store}
[ "$2" = default ] || exit 0
if [ "$3" = -d ]; then rm -f "$store"; exit 0; fi
[ "$4" = default.configured.audio.source ] || exit 0
if [ $# -ge 5 ]; then printf '%s' "$5" > "$store"; exit 0; fi
[ -f {hang} ] && exec sleep 10
echo 'Found "default" metadata 34'
[ -f "$store" ] && echo "update: id:0 key:'default.configured.audio.source' value:'$(cat "$store")' type:'Spa:String:JSON'"
exit 0"#,
            store = store.display(),
            hang = hang.display(),
        ),
    );
    for tool in ["pw-cli", "pw-link", "wpctl"] {
        script(&bin, tool, "exit 0");
    }
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.display()));
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("XDG_STATE_HOME", &state);
    std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
    std::env::set_var("PIPEWIRE_REMOTE", "hushmic-test-no-daemon");
    for f in [
        "plugin.so",
        "libonnxruntime.so",
        "models/dpdfnet8_48khz_hr.onnx",
    ] {
        std::fs::write(root.join(f), b"").unwrap();
    }
    let cfg = Config {
        set_default: true,
        ..Config::default()
    };
    let mut c = Controller::new(Paths {
        plugin_so: root.join("plugin.so"),
        model_dir: root.join("models"),
        dylib: root.join("libonnxruntime.so"),
    });
    // A hanging call ends at the cap. The caps hold for every attempt of
    // this controller (they are the worker's), so the later attempts that
    // must land run under them too: room for a stand-in call that a
    // starved CI pod answers late. With 800 ms a read-back over it twice
    // left the write unconfirmed and the takeover not landed.
    c.set_takeover_caps(Caps {
        call: Duration::from_secs(3),
        attempt: Duration::from_secs(8),
    });
    c.enable(&cfg).unwrap();
    assert!(
        !c.default_taken(),
        "enable() leaves the takeover to the node's arrival"
    );

    // The node is up, but the session manager does not answer the read.
    std::fs::write(&hang, b"").unwrap();
    c.ensure_default_takeover(&cfg, true);
    // The caller is back while the attempt is still out: its read hangs
    // until the cap, seconds from here.
    assert!(
        !c.finish_takeover(Duration::ZERO),
        "the attempt held the caller"
    );
    finish(&mut c);
    assert!(
        !c.default_taken(),
        "nothing was captured, so nothing is taken"
    );
    assert_eq!(default_source(&store).as_deref(), Some("real_mic"));

    // It answers again: the takeover records the user's default first.
    std::fs::remove_file(&hang).unwrap();
    c.ensure_default_takeover(&cfg, true);
    finish(&mut c);
    assert!(c.default_taken() && c.default_landed());
    assert_eq!(default_source(&store).as_deref(), Some("hushmic_source"));

    // Turning "Set as default" off hands it back at once: the host (and
    // hushmic_source) stays, only the chain restarts.
    let off = Config {
        set_default: false,
        ..cfg.clone()
    };
    c.apply_settings(&off).unwrap();
    assert!(c.host_running(), "the setting restarts the chain only");
    assert!(!c.default_taken());
    assert_eq!(default_source(&store).as_deref(), Some("real_mic"));

    // On again: taken again, and quitting hands it back, never deletes it.
    c.apply_settings(&cfg).unwrap();
    c.ensure_default_takeover(&cfg, true);
    finish(&mut c);
    assert!(c.default_taken() && c.default_landed());
    assert_eq!(default_source(&store).as_deref(), Some("hushmic_source"));
    c.disable().unwrap();
    assert_eq!(default_source(&store).as_deref(), Some("real_mic"));

    drop(c);
    if let Some(d) = hushmic::meter::dir_for(Some(&root.join("run"))) {
        if !d.starts_with(&root) {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&root);
}
