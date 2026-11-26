//! A private session bus for the D-Bus tests: its own `dbus-daemon` with
//! no service directories, so a name the test leaves unowned (a portal
//! that is "not there yet") is never auto-started from the host's service
//! files (a stock session config would launch the real portal).

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

pub struct PrivateBus {
    child: Child,
    /// The bus address (`unix:path=…`).
    pub addr: String,
    /// A scratch dir for the test, removed with the bus.
    pub dir: PathBuf,
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Start the bus; None (with a SKIP line naming `test`) when there is no
/// usable `dbus-daemon`.
pub fn private_bus(test: &str) -> Option<PrivateBus> {
    if Command::new("dbus-daemon")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP {test}: dbus-daemon not available");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("hushmic-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let conf = dir.join("bus.conf");
    std::fs::write(
        &conf,
        format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
            dir.display()
        ),
    )
    .ok()?;
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", conf.display()))
        .args(["--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dbus-daemon");
    let addr = {
        let out = child.stdout.take().expect("dbus-daemon stdout");
        let mut line = String::new();
        let _ = BufReader::new(out).read_line(&mut line);
        line.trim().to_string()
    };
    let bus = PrivateBus { child, addr, dir };
    if !bus.addr.starts_with("unix:") {
        eprintln!("SKIP {test}: no usable bus address ({:?})", bus.addr);
        return None;
    }
    Some(bus)
}
