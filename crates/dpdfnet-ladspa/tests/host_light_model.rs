//! Which models a host gets when it instantiates the plugin the way
//! EasyEffects does: through the LADSPA descriptor, with none of the
//! variables the HushMic app sets. The model paths and the ONNX Runtime
//! commit are process-global, so this harness-less test re-executes itself,
//! one subprocess per case with a controlled environment, and reads the
//! plugin's stderr. Asset-gated like the other suites.
//!
//! On the timing sweep's realtime list, which allows a 25% CPU quota. Its
//! CPU check judges a hang by CPU used, not by a clock, so the sweep proves
//! it at the 6% the other suites get; a failure there is a real finding.

mod common;

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The variables the app sets for its chain; every case starts without them.
const HOST_VARS: [&str; 7] = [
    "HUSHMIC_MODEL_PATH",
    "HUSHMIC_FALLBACK_MODEL_PATH",
    "HUSHMIC_INFERENCE",
    "HUSHMIC_DSP_TIER",
    "HUSHMIC_DSP_DWELL_HOPS",
    "HUSHMIC_VOICE_HOLD",
    "ORT_DYLIB_PATH",
];

fn main() {
    if std::env::var_os("HUSHMIC_HOST_CASE").is_some() {
        // One instance at 48 kHz: init_engine runs on this thread.
        let d = dpdfnet_ladspa::get_ladspa_descriptor(0).expect("descriptor 0");
        let plugin = (d.new)(&d, 48_000);
        drop(plugin);
        return;
    }
    let (Some(quality), Some(light), Some(runtime)) = (
        common::model_path("dpdfnet8_48khz_hr.onnx"),
        common::model_path("dpdfnet2_48khz_hr.onnx"),
        common::runtime_path(),
    ) else {
        println!("SKIP host_light_model: dev assets not provisioned");
        return;
    };
    let utf8 = |p: &std::path::Path| p.to_str().expect("utf-8 path").to_owned();
    let (quality_s, light_s, runtime_s) = (utf8(&quality), utf8(&light), utf8(&runtime));
    let mut cases = 0;

    // No variables at all (EasyEffects): the compiled-in defaults, which
    // are the dev assets unless the build baked install paths
    // (HUSHMIC_BUILD_MODEL / HUSHMIC_BUILD_DYLIB).
    let same = |a: &str, b: &std::path::Path| {
        std::path::Path::new(a).canonicalize().ok() == b.canonicalize().ok()
    };
    if same(env!("HUSHMIC_DEFAULT_MODEL"), &quality)
        && same(env!("HUSHMIC_DEFAULT_DYLIB"), &runtime)
    {
        let log = case::<&str>("bare host", &[]);
        assert!(log.contains("for dpdfnet8_48khz_hr\n"), "{log}");
        assert!(log.contains("for dpdfnet2_48khz_hr\n"), "{log}");
        assert!(log.contains("(next to the main model)"), "{log}");

        // The ladder has the light tier: pinning it is accepted.
        let log = case("pinned light", &[("HUSHMIC_DSP_TIER", "light")]);
        assert!(!log.contains("ignored"), "{log}");
        cases += 2;
    } else {
        println!("SKIP bare host cases: the build baked other default paths");
    }

    // The main model and the runtime named, as a host wrapper would.
    let named = |extra: &[(&'static str, &str)]| {
        let mut env = vec![
            ("HUSHMIC_MODEL_PATH", quality_s.as_str()),
            ("ORT_DYLIB_PATH", runtime_s.as_str()),
        ];
        env.extend_from_slice(extra);
        env.into_iter()
            .map(|(k, v)| (k, v.to_owned()))
            .collect::<Vec<_>>()
    };

    // Empty counts as unset.
    let log = case("empty name", &named(&[("HUSHMIC_FALLBACK_MODEL_PATH", "")]));
    assert!(log.contains("for dpdfnet2_48khz_hr\n"), "{log}");

    // A named path wins, even one that does not load.
    let log = case(
        "named path",
        &named(&[("HUSHMIC_FALLBACK_MODEL_PATH", "/nonexistent/light.onnx")]),
    );
    assert!(log.contains("light model unavailable"), "{log}");
    assert!(!log.contains("for dpdfnet2_48khz_hr\n"), "{log}");

    // Pinned to the main model: the light one is not loaded (issue #36).
    let log = case("pinned quality", &named(&[("HUSHMIC_DSP_TIER", "quality")]));
    assert_eq!(log.matches("inference: ").count(), 1, "{log}");
    assert!(log.contains("the light model is not loaded"), "{log}");
    assert!(!log.contains("ignored"), "{log}");

    // The voice hold is off unless the host's environment turns it on.
    let log = case("no voice hold", &named(&[]));
    assert!(!log.contains("voice hold"), "{log}");
    let log = case("voice hold", &named(&[("HUSHMIC_VOICE_HOLD", "1")]));
    assert!(log.contains("voice hold: on"), "{log}");

    // A light main model loads once and has no fallback.
    let mut env = named(&[]);
    env[0].1 = light_s.clone();
    let log = case("light main", &env);
    assert_eq!(log.matches("inference: ").count(), 1, "{log}");
    assert!(!log.contains("light model"), "{log}");

    // No sibling: one line, the ladder is [Quality, Raw] as before.
    let dir = std::env::temp_dir().join(format!("hushmic-host-light-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let lone = dir.join("dpdfnet8_48khz_hr.onnx");
    std::os::unix::fs::symlink(&quality, &lone).expect("symlink");
    let mut env = named(&[("HUSHMIC_DSP_TIER", "light")]);
    env[0].1 = utf8(&lone);
    let log = case("no sibling", &env);
    assert!(log.contains("no light model at"), "{log}");
    assert!(!log.contains("for dpdfnet2_48khz_hr\n"), "{log}");
    assert!(log.contains("HUSHMIC_DSP_TIER=light ignored"), "{log}");

    // A sibling that is the main model itself is not loaded twice.
    std::os::unix::fs::symlink(&quality, dir.join("dpdfnet2_48khz_hr.onnx")).expect("symlink");
    let log = case("sibling is the main model", &env);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(log.contains("is the main model itself"), "{log}");
    assert!(!log.contains("(next to the main model)"), "{log}");
    assert_eq!(log.matches("inference: ").count(), 1, "{log}");
    cases += 8;
    println!("host_light_model: {cases} cases ok");
}

/// Run one instantiation in a child and return its stderr.
fn case<V: AsRef<std::ffi::OsStr>>(name: &str, env: &[(&str, V)]) -> String {
    let mut cmd = Command::new(std::env::current_exe().expect("current_exe"));
    for v in HOST_VARS {
        cmd.env_remove(v);
    }
    cmd.env("HUSHMIC_HOST_CASE", name);
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Bounded: a hung init must fail with a name, not stall the job.
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn case");
    let mut stderr = child.stderr.take().expect("stderr pipe");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    // Hung means no CPU: a case that still runs (a slow init on a loaded
    // runner) is waited for, one that used no CPU for a minute is not.
    let mut cpu = (cpu_ticks(child.id()), Instant::now());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait on case") {
            break status;
        }
        let now = cpu_ticks(child.id());
        if now != cpu.0 {
            cpu = (now, Instant::now());
        }
        let idle = cpu.1.elapsed() >= Duration::from_secs(60);
        if idle || started.elapsed() >= Duration::from_secs(30 * 60) {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "case {name} timed out ({})",
                if idle { "no CPU for 60 s" } else { "30 min" }
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let log = reader.join().expect("stderr reader");
    assert!(status.success(), "case {name} failed:\n{log}");
    log
}

/// The CPU a process has used so far (user + system clock ticks; None
/// once it is gone).
fn cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the command's closing parenthesis: utime and stime are
    // the 12th and 13th of them.
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}
