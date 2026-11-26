//! Starting this program again: the window and the file workers.
//!
//! They run `/proc/self/exe`, the very binary the daemon runs, not the
//! file at its path: after an upgrade that path holds the new version (or,
//! replaced, reads "… (deleted)" and cannot start at all), and a child
//! must match the daemon it talks to. `argv[0]` stays the path, and the
//! child names itself `hushmic` again ([`restore_name`]): started through
//! `/proc/self/exe`, the kernel would call it "exe", and `ps`, `pgrep -x
//! hushmic` and the desktop's task lists go by that name.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const SELF: &str = "/proc/self/exe";
const NAME: &str = "hushmic";

/// A command that starts this program again (see the module docs).
pub fn command() -> Command {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().ok();
    let program = if Path::new(SELF).exists() {
        PathBuf::from(SELF)
    } else {
        exe.clone().unwrap_or_else(|| NAME.into())
    };
    let mut c = Command::new(program);
    c.arg0(exe.map_or_else(|| OsString::from(NAME), |p| arg0(&p)));
    c
}

/// The path as `argv[0]`, without the " (deleted)" Linux adds to the link
/// of a binary that was replaced.
fn arg0(exe: &Path) -> OsString {
    let s = exe.as_os_str().to_string_lossy();
    match s.strip_suffix(" (deleted)") {
        Some(p) => p.into(),
        None => exe.as_os_str().to_owned(),
    }
}

/// Started through `/proc/self/exe`, this process is called "exe": call it
/// `hushmic` again. A process started any other way keeps its name.
pub fn restore_name() {
    let comm = std::fs::read_to_string("/proc/self/comm").unwrap_or_default();
    if comm.trim_end() == "exe" {
        // SAFETY: PR_SET_NAME reads a NUL-terminated string of at most 16
        // bytes.
        unsafe { libc::prctl(libc::PR_SET_NAME, c"hushmic".as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv0_is_the_path_without_the_deleted_mark() {
        assert_eq!(
            arg0(Path::new("/usr/bin/hushmic (deleted)")),
            "/usr/bin/hushmic"
        );
        assert_eq!(arg0(Path::new("/usr/bin/hushmic")), "/usr/bin/hushmic");
    }

    #[test]
    fn it_starts_the_running_binary_under_its_own_path() {
        // This test binary, again: it lists its tests and ends.
        let out = command()
            .args(["--list", "--format", "terse"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("selfexe::tests::"));
    }

    #[test]
    fn a_replaced_binary_still_starts_through_proc_self_exe() {
        // The kernel side of it, with a shell that removes its own file
        // and then starts itself again.
        let dir = std::env::temp_dir().join(format!("hushmic-selfexe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = dir.join("sh");
        std::fs::copy("/bin/sh", &sh).unwrap();
        let out = Command::new(&sh)
            .args(["-c", "rm \"$0\" && exec /proc/self/exe -c 'echo started'"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "started");
        let _ = std::fs::remove_dir_all(dir);
    }
}
