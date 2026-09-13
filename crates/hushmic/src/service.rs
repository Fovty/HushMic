//! The systemd user unit: the packaged file for /usr installs, and a
//! generated copy under ~/.config/systemd/user for installs systemd cannot
//! see (AppImage, $HOME prefixes, Nix store paths).

use crate::autostart::LaunchSpec;
use std::path::PathBuf;
use std::process::Command;

/// systemd's own quoting (systemd.syntax(7)): double quotes, backslash
/// escapes for `\` and `"`, and `%` doubled because ExecStart= and
/// Environment= expand specifiers.
fn sd_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The unit text for one install. With the packaged binary and no env
/// overrides this is byte-identical to packaging/systemd/hushmic.service
/// (asserted by a test), so the two never drift.
pub fn unit_contents(spec: &LaunchSpec) -> String {
    let exec = if spec.program == "/usr/bin/hushmic" {
        spec.program.clone()
    } else {
        sd_quote(&spec.program)
    };
    let env: String = spec
        .env
        .iter()
        .map(|(k, v)| format!("Environment={}\n", sd_quote(&format!("{k}={v}"))))
        .collect();
    // KillMode=mixed: the `pipewire -c` child is in the unit's cgroup, and
    // the default kill would SIGTERM it at the same instant as the daemon,
    // racing the orderly teardown that restores the previous default mic.
    // Restart=on-failure, not always: "already running" exits 0 and
    // `always` would loop. default.target, not graphical-session.target:
    // that one stops at logout, and this daemon has no display to lose.
    format!(
        "[Unit]
Description=HushMic noise-suppression virtual microphone
Documentation=https://github.com/Fovty/hushmic
After=pipewire.service wireplumber.service
Wants=pipewire.service

[Service]
ExecStart={exec} --headless
{env}Restart=on-failure
RestartSec=3
KillMode=mixed
TimeoutStopSec=10
Slice=session.slice

[Install]
WantedBy=default.target
"
    )
}

pub fn user_unit_path() -> Result<PathBuf, String> {
    Ok(directories::BaseDirs::new()
        .ok_or("no home directory")?
        .config_dir()
        .join("systemd/user/hushmic.service"))
}

/// Where systemd currently loads `hushmic.service` from, if anywhere.
fn fragment_path() -> Option<PathBuf> {
    let out = systemctl(&["show", "-P", "FragmentPath", "hushmic.service"]).ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !p.is_empty()).then(|| PathBuf::from(p))
}

fn systemctl(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl --user is not available: {e}"))
}

/// `systemctl --user is-enabled hushmic.service` says enabled (packaged or
/// generated unit alike). False whenever systemd cannot be asked.
pub fn unit_enabled() -> bool {
    if crate::sandbox::is_flatpak() {
        return false;
    }
    systemctl(&["is-enabled", "--quiet", "hushmic.service"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn install() -> Result<String, String> {
    if crate::sandbox::is_flatpak() {
        return Err("inside Flatpak, write a unit on the host instead — see \
                    \"Starting at login\" in the README"
            .into());
    }
    let spec = crate::autostart::launch_spec();
    let path = user_unit_path()?;
    // A packaged unit (deb/rpm/AUR/tarball) already fits this install; a
    // generated copy under ~/.config would shadow it and go stale on the
    // next package update.
    if let Some(p) = fragment_path() {
        if p != path && p.starts_with("/usr") {
            return Err(format!(
                "this install already ships a unit at {}; enable it with:  \
                 systemctl --user enable --now hushmic.service",
                p.display()
            ));
        }
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("could not create {}: {e}", d.display()))?;
    }
    crate::fsutil::atomic_write(&path, unit_contents(&spec).as_bytes())
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    match systemctl(&["daemon-reload"]) {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            return Err(format!(
                "wrote {}, but systemctl --user daemon-reload failed: {}",
                path.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            ))
        }
        Err(e) => return Err(format!("wrote {}, but {e}", path.display())),
    }
    // Turning the login autostart entry off (the unit and the entry would
    // race for the lock at every login) is the caller's job: it must go
    // through the running daemon when there is one — the daemon is the
    // config file's only writer while it runs.
    Ok(format!(
        "wrote {}\nenable it with:  systemctl --user enable --now hushmic.service\n\
         start at boot without logging in:  loginctl enable-linger\n",
        path.display()
    ))
}

pub fn uninstall() -> Result<String, String> {
    if crate::sandbox::is_flatpak() {
        return Err("inside Flatpak there is no generated unit to remove".into());
    }
    let path = user_unit_path()?;
    if !path.exists() {
        return Ok(format!(
            "nothing to remove: {} does not exist\n",
            path.display()
        ));
    }
    let _ = systemctl(&["disable", "--now", "hushmic.service"]);
    std::fs::remove_file(&path).map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    let _ = systemctl(&["daemon-reload"]);
    Ok(format!("removed {} (unit disabled)\n", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_text_matches_the_packaged_unit_for_a_usr_install() {
        let spec = LaunchSpec {
            program: "/usr/bin/hushmic".into(),
            env: vec![],
        };
        let packaged = include_str!("../../../packaging/systemd/hushmic.service");
        assert_eq!(unit_contents(&spec), packaged);
    }

    #[test]
    fn unit_quotes_paths_and_carries_env_overrides() {
        let spec = LaunchSpec {
            program: "/home/u/Apps/Hush Mic%1.AppImage".into(),
            env: vec![(
                "HUSHMIC_MODEL_DIR".into(),
                "/home/u/.local/share/hushmic/models".into(),
            )],
        };
        let u = unit_contents(&spec);
        assert!(
            u.contains("ExecStart=\"/home/u/Apps/Hush Mic%%1.AppImage\" --headless\n"),
            "{u}"
        );
        assert!(
            u.contains("Environment=\"HUSHMIC_MODEL_DIR=/home/u/.local/share/hushmic/models\"\n"),
            "{u}"
        );
        assert!(u.contains("KillMode=mixed\n"));
        assert_eq!(sd_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn user_unit_path_is_under_config() {
        assert!(user_unit_path()
            .unwrap()
            .ends_with("systemd/user/hushmic.service"));
    }
}
