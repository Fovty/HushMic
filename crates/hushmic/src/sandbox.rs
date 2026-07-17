//! Flatpak sandbox detection.
//!
//! HushMic runs unchanged inside a Flatpak EXCEPT where the sandbox changes
//! the rules of the outside world: the SNI well-known bus name cannot be
//! owned (the session-bus proxy only allows names under the app ID), host
//! icon themes only resolve names the flatpak EXPORTS (app-ID-prefixed),
//! `~/.config/autostart` is private to the sandbox (the Background portal is
//! the host-visible channel), and WirePlumber silently discards metadata
//! writes from flatpak-flagged clients. Each affected call site gates on the
//! probes here; everything else runs the exact same code as a native install.

use std::sync::OnceLock;

/// True when running inside a Flatpak sandbox. `/.flatpak-info` is the
/// canonical marker: flatpak mounts it read-only into every instance, and
/// PipeWire itself detects flatpak clients by statting this very file
/// through `/proc/<pid>/root` — so this probe and the daemon's view of us
/// cannot disagree.
pub fn is_flatpak() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::path::Path::new("/.flatpak-info").exists())
}

/// The Flatpak app ID (e.g. `io.github.fovty.HushMic`); `None` outside a
/// sandbox. Read from `$FLATPAK_ID` (exported into every app instance), with
/// `/.flatpak-info`'s `[Application] name=` as fallback so icon naming keeps
/// working even if a future flatpak stops exporting the env var.
pub fn flatpak_app_id() -> Option<&'static str> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| {
        if !is_flatpak() {
            return None;
        }
        match std::env::var("FLATPAK_ID") {
            Ok(id) if !id.is_empty() => Some(id),
            _ => std::fs::read_to_string("/.flatpak-info")
                .ok()
                .and_then(|s| parse_flatpak_info_name(&s)),
        }
    })
    .as_deref()
}

/// `[Application] name=` from flatpak-info keyfile text. Pure for testability.
pub fn parse_flatpak_info_name(text: &str) -> Option<String> {
    let mut in_application = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_application = section == "Application";
            continue;
        }
        if in_application {
            if let Some(v) = line.strip_prefix("name=") {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_application_name() {
        let info = "[Application]\nname=io.github.fovty.HushMic\nruntime=runtime/x/y/z\n";
        assert_eq!(
            parse_flatpak_info_name(info),
            Some("io.github.fovty.HushMic".to_string())
        );
    }

    #[test]
    fn name_outside_the_application_section_is_ignored() {
        // [Instance] also carries keys; a name-ish key there must not win.
        let info = "[Context]\nname=wrong\n[Application]\nname=right\n[Instance]\nname=alsowrong\n";
        assert_eq!(parse_flatpak_info_name(info), Some("right".to_string()));
    }

    #[test]
    fn missing_or_empty_name_is_none() {
        assert_eq!(parse_flatpak_info_name("[Application]\nruntime=x\n"), None);
        assert_eq!(parse_flatpak_info_name("[Application]\nname=\n"), None);
        assert_eq!(parse_flatpak_info_name(""), None);
    }
}
