//! ONNX-runtime lifecycle sequences. The environment commit is process-global
//! and first-wins, so every sequence needs a fresh process with a controlled
//! environment: this harness-less test re-executes itself, one subprocess per
//! case (`HUSHMIC_RT_CASE` selects the role).
//!
//! Sequences that require the bare-soname fallback to FAIL cannot assert on a
//! machine with a distro `libonnxruntime.so` in the default dlopen search
//! path (no env var can hide it) — a probe subprocess detects that and those
//! cases SKIP; CI is the known-clean environment where they always run.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() {
    if let Ok(case) = std::env::var("HUSHMIC_RT_CASE") {
        run_case(&case);
        return;
    }
    orchestrate();
}

// ---------------------------------------------------------------- child roles

fn run_case(case: &str) {
    use hushmic_denoiser::{init_runtime, Denoiser, Error, RuntimeInit};
    let model = std::env::var("HUSHMIC_RT_MODEL").unwrap_or_default();
    let dylib = std::env::var("HUSHMIC_RT_DYLIB").unwrap_or_default();
    match case {
        // exit 0 = a bare-soname runtime IS loadable on this machine
        "probe" => {
            let loadable = init_runtime("libonnxruntime.so").is_ok();
            std::process::exit(if loadable { 0 } else { 10 });
        }
        // no env var, no system ORT: construction must fail with Error::Runtime
        "no_runtime" => match Denoiser::from_file(&model) {
            Err(Error::Runtime(_)) => println!("CASE-OK"),
            Err(e) => panic!("expected Error::Runtime, got {e:?}"),
            Ok(_) => panic!("expected Error::Runtime, got a Denoiser"),
        },
        // the no-latch rule: a failed implicit init must not poison a later
        // explicit init_runtime with a valid path
        "recover" => {
            match Denoiser::from_file(&model) {
                Err(Error::Runtime(_)) => {}
                Err(e) => panic!("expected initial Error::Runtime, got {e:?}"),
                Ok(_) => panic!("expected initial Error::Runtime, got a Denoiser"),
            }
            match init_runtime(&dylib) {
                Ok(RuntimeInit::Committed) => {}
                other => panic!("expected Committed after failed implicit init, got {other:?}"),
            }
            assert!(
                Denoiser::from_file(&model).is_ok(),
                "explicit init after failed implicit init must yield a working Denoiser"
            );
            println!("CASE-OK");
        }
        // ORT_DYLIB_PATH alone makes implicit construction work
        "env_var" => {
            assert!(
                Denoiser::from_file(&model).is_ok(),
                "ORT_DYLIB_PATH must drive the implicit init"
            );
            println!("CASE-OK");
        }
        // idempotency + the AlreadyInitialized signal
        "twice" => {
            match init_runtime(&dylib) {
                Ok(RuntimeInit::Committed) => {}
                other => panic!("first init must commit, got {other:?}"),
            }
            match init_runtime(&dylib) {
                Ok(RuntimeInit::AlreadyInitialized) => {}
                other => panic!("second init must report AlreadyInitialized, got {other:?}"),
            }
            println!("CASE-OK");
        }
        other => panic!("unknown case {other}"),
    }
}

// ---------------------------------------------------------------- orchestrator

/// Spawn ourselves as `case`; ORT_DYLIB_PATH is always scrubbed first and
/// only set when the case calls for it. Bounded by a kill timeout: the exact
/// regression this suite guards against (a reintroduced init deadlock) shows
/// up as an infinite child futex-wait, which must fail with a name instead of
/// hanging the whole test binary until the CI job limit.
fn spawn(case: &str, env: &[(&str, &str)]) -> (bool, String) {
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(exe);
    cmd.env_remove("ORT_DYLIB_PATH")
        .env("HUSHMIC_RT_CASE", case);
    for (k, v) in env {
        cmd.env(k, v);
    }
    run_with_timeout(cmd, 60)
}

fn run_with_timeout(mut cmd: Command, secs: u64) -> (bool, String) {
    use std::io::Read;
    use std::process::Stdio;
    // stdout is a handful of marker bytes, far below the pipe buffer, so
    // reading it only after exit cannot block the child.
    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
    let mut child = cmd.spawn().expect("spawn case");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait().expect("wait on child") {
            Some(status) => {
                let mut s = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut s);
                }
                return (status.success(), s);
            }
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return (false, "TIMED OUT (possible init deadlock)".to_string());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

fn orchestrate() {
    // CI exports HUSHMIC_ASSERT_ASSETS=1: there, every skip is a provisioning
    // or environment regression and must fail loudly.
    let assert_env = std::env::var("HUSHMIC_ASSERT_ASSETS").as_deref() == Ok("1");
    let model = repo_root().join("assets/models/dpdfnet8_48khz_hr.onnx");
    let dylib = repo_root().join("assets/lib/libonnxruntime.so");
    if !model.exists() || !dylib.exists() {
        assert!(
            !assert_env,
            "dev assets missing but HUSHMIC_ASSERT_ASSETS=1"
        );
        println!("SKIP runtime_lifecycle: dev assets not provisioned");
        return;
    }
    let model = model.display().to_string();
    let dylib = dylib.display().to_string();

    let mut failures = Vec::new();
    let mut check = |name: &str, (ok, stdout): (bool, String)| {
        if ok && stdout.contains("CASE-OK") {
            println!("ok   {name}");
        } else {
            failures.push(name.to_string());
            println!("FAIL {name}: success={ok} stdout={stdout:?}");
        }
    };

    // Does this machine resolve a bare-soname ONNX Runtime? Then the
    // fallback-must-fail sequences cannot assert here.
    let exe = std::env::current_exe().expect("current_exe");
    let mut probe = Command::new(exe);
    probe
        .env_remove("ORT_DYLIB_PATH")
        .env("HUSHMIC_RT_CASE", "probe");
    let (system_ort, _) = run_with_timeout(probe, 60);

    if system_ort {
        assert!(
            !assert_env,
            "a system libonnxruntime.so is dlopen-able but HUSHMIC_ASSERT_ASSETS=1 \
             (CI must stay clean so the fallback-failure sequences assert)"
        );
        println!("SKIP no_runtime, recover: a system libonnxruntime.so is dlopen-able here");
    } else {
        check(
            "no_runtime",
            spawn("no_runtime", &[("HUSHMIC_RT_MODEL", &model)]),
        );
        check(
            "recover",
            spawn(
                "recover",
                &[("HUSHMIC_RT_MODEL", &model), ("HUSHMIC_RT_DYLIB", &dylib)],
            ),
        );
    }
    check(
        "env_var",
        spawn(
            "env_var",
            &[("HUSHMIC_RT_MODEL", &model), ("ORT_DYLIB_PATH", &dylib)],
        ),
    );
    check("twice", spawn("twice", &[("HUSHMIC_RT_DYLIB", &dylib)]));

    if !failures.is_empty() {
        panic!("runtime lifecycle failures: {failures:?}");
    }
}
