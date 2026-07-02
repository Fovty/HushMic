use directories::BaseDirs;
use std::path::PathBuf;

/// Quote one Exec= argument per the Desktop Entry spec, applying BOTH layers:
/// the Exec quoting rule (whole-argument double quotes; `"`, `` ` ``, `$`, `\`
/// backslash-escaped) and then the key-file string escape on top (every
/// backslash doubled) — parsers unescape key-file first, then Exec. Emitting
/// only the Exec layer produces sequences like `\$` that GKeyFile rejects as
/// invalid string escapes, so the entry would silently never launch.
fn quote_exec_arg(s: &str) -> String {
    let mut exec = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                exec.push('\\');
                exec.push(c);
            }
            // Field codes are expanded even inside quotes: a literal `%` must
            // be written `%%` or the launcher mangles the argument.
            '%' => exec.push_str("%%"),
            c => exec.push(c),
        }
    }
    exec.push('"');
    exec.replace('\\', "\\\\")
}

/// Build the `Exec=` line for the autostart entry.
///
/// When running as an AppImage the live binary is at an ephemeral mount
/// (`/tmp/.mount_*`), so the autostart entry must point at the AppImage *file*
/// instead — its runtime exports `$APPIMAGE` with that absolute path, and its
/// AppRun re-exports the asset env vars, so nothing else needs baking. (If the
/// user later moves the AppImage, the entry goes stale; that's inherent to the
/// AppImage format.)
///
/// For everything else the entry is executed by the session manager with a
/// minimal environment: no login-shell PATH and none of the exports install.sh
/// prints for non-/usr prefixes. So bake in the absolute binary path and any
/// HUSHMIC_*/ORT_DYLIB_PATH overrides active right now — otherwise a
/// `--prefix $HOME/.local` install autostarts into a binary that either isn't
/// found or can't find its assets.
///
/// Pure helper so the path/env logic is unit-testable without touching the env.
fn exec_field_for(
    appimage: Option<&str>,
    exe: Option<&str>,
    env_overrides: &[(&str, String)],
) -> String {
    if let Some(p) = appimage {
        if !p.is_empty() {
            return format!("{} --tray", quote_exec_arg(p));
        }
    }
    let cmd = match exe {
        Some(e) if !e.is_empty() => quote_exec_arg(e),
        _ => "hushmic".to_string(),
    };
    if env_overrides.is_empty() {
        format!("{cmd} --tray")
    } else {
        let vars = env_overrides
            .iter()
            .map(|(k, v)| quote_exec_arg(&format!("{k}={v}")))
            .collect::<Vec<_>>()
            .join(" ");
        format!("env {vars} {cmd} --tray")
    }
}

fn exec_field() -> String {
    let appimage = std::env::var("APPIMAGE").ok();
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string());
    let mut envs: Vec<(&str, String)> = Vec::new();
    for k in ["HUSHMIC_PLUGIN_SO", "HUSHMIC_MODEL_DIR", "ORT_DYLIB_PATH"] {
        if let Ok(v) = std::env::var(k) {
            envs.push((k, v));
        }
    }
    exec_field_for(appimage.as_deref(), exe.as_deref(), &envs)
}

/// The full `hushmic.desktop` autostart entry, with the right `Exec=` for how
/// this build was launched (installed command vs AppImage path).
pub fn desktop_contents() -> String {
    format!(
        "[Desktop Entry]
Type=Application
Name=HushMic
Comment=Real-time microphone noise suppression
Exec={exec}
Icon=hushmic
Terminal=false
Categories=AudioVideo;Audio;
X-GNOME-Autostart-enabled=true
",
        exec = exec_field()
    )
}

pub fn desktop_path() -> PathBuf {
    BaseDirs::new()
        .expect("home")
        .config_dir()
        .join("autostart")
        .join("hushmic.desktop")
}

pub fn is_autostart_enabled() -> bool {
    desktop_path().exists()
}

pub fn set_autostart(enabled: bool) -> std::io::Result<()> {
    let p = desktop_path();
    if enabled {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, desktop_contents())
    } else if p.exists() {
        std::fs::remove_file(p)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_path_is_in_autostart() {
        let p = desktop_path();
        assert!(
            p.ends_with("autostart/hushmic.desktop"),
            "unexpected path: {p:?}"
        );
    }

    #[test]
    fn installed_exec_uses_absolute_binary_path() {
        // Session managers don't have the login shell's PATH; the entry must
        // point at the real binary when we know it.
        assert_eq!(
            exec_field_for(None, Some("/usr/bin/hushmic"), &[]),
            "\"/usr/bin/hushmic\" --tray"
        );
        // Only when the executable path is unknowable do we fall back to PATH.
        assert_eq!(exec_field_for(None, None, &[]), "hushmic --tray");
        assert_eq!(exec_field_for(Some(""), None, &[]), "hushmic --tray");
    }

    #[test]
    fn appimage_exec_points_at_the_appimage_file() {
        assert_eq!(
            exec_field_for(Some("/home/u/Apps/HushMic.AppImage"), None, &[]),
            "\"/home/u/Apps/HushMic.AppImage\" --tray"
        );
    }

    #[test]
    fn env_overrides_are_baked_into_exec() {
        // A $HOME/.local install runs with exported HUSHMIC_* vars; the
        // autostart entry must carry them since the session manager won't.
        let envs = [(
            "HUSHMIC_MODEL_DIR",
            "/home/u/.local/share/hushmic/models".to_string(),
        )];
        assert_eq!(
            exec_field_for(None, Some("/home/u/.local/bin/hushmic"), &envs),
            "env \"HUSHMIC_MODEL_DIR=/home/u/.local/share/hushmic/models\" \
             \"/home/u/.local/bin/hushmic\" --tray"
        );
    }

    #[test]
    fn exec_args_are_spec_quoted() {
        // Reserved chars get the Exec-layer backslash escape AND the key-file
        // layer's backslash doubling on top (parsers unescape key-file first):
        // a literal `$` must appear as `\\$` in the file bytes.
        assert_eq!(quote_exec_arg("/pa th/$x\"y"), "\"/pa th/\\\\$x\\\\\"y\"");
        // plain paths stay untouched apart from the surrounding quotes
        assert_eq!(quote_exec_arg("/usr/bin/hushmic"), "\"/usr/bin/hushmic\"");
        // literal % must not be read as an Exec field code
        assert_eq!(quote_exec_arg("/mnt/50%off/x"), "\"/mnt/50%%off/x\"");
    }

    #[test]
    fn contents_are_a_valid_desktop_entry() {
        let c = desktop_contents();
        assert!(c.contains("Type=Application"));
        assert!(c.contains("Name=HushMic"));
        assert!(c.contains("--tray"));
    }
}
