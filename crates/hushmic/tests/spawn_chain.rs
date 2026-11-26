//! `spawn_chain`, the one start both chains share, against a stand-in
//! `pipewire` that records its arguments and environment: the mode and
//! strength the control file opens with (a microphone started muted whose
//! file said 0 would be a hot mic), each chain's own meter, control and log
//! files and generation, the plugin environment, and the light fallback.
//!
//! One test: it sets PATH and the XDG directories for this process.

use hushmic::config::Inference;
use hushmic::controller::{spawn_chain, Paths, RunMode, SpawnSpec};
use hushmic::diagnostics::{engine_generation_of, Chain};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FAKE: &str = "#!/bin/sh
# argv: -c CONF. Leave what this host was started with next to its conf.
env > \"$2.env.part\" && mv \"$2.env.part\" \"$2.env\"
echo \"stand-in host for $2\" >&2
exec sleep 30
";

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn env_of(conf: &Path) -> HashMap<String, String> {
    let file = PathBuf::from(format!("{}.env", conf.display()));
    wait_for("the host's environment", || file.exists());
    std::fs::read_to_string(&file)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn mode_in(control: &Path) -> (Option<u32>, Option<f32>) {
    let r = hushmic_shm::control::ControlReader::open(control).expect("a control file");
    (r.mode(), r.attn_db())
}

#[test]
fn each_chain_starts_with_its_own_files_mode_and_environment() {
    let root = PathBuf::from(format!("/dev/shm/hm-spawn-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bin = root.join("bin");
    let run = root.join("run");
    let state = root.join("state");
    let models = root.join("models");
    for d in [&bin, &run, &state, &models] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::set_permissions(&run, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let fake = bin.join("pipewire");
    std::fs::write(&fake, FAKE).unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    for m in ["dpdfnet8_48khz_hr", "dpdfnet2_48khz_hr"] {
        std::fs::write(models.join(format!("{m}.onnx")), b"").unwrap();
    }
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    std::env::set_var("XDG_RUNTIME_DIR", &run);
    std::env::set_var("XDG_STATE_HOME", &state);
    std::env::remove_var("HUSHMIC_INFERENCE");
    std::env::remove_var("HUSHMIC_VOICE_HOLD");
    let paths = Paths {
        plugin_so: root.join("libdpdfnet_ladspa.so"),
        model_dir: models.clone(),
        dylib: root.join("libonnxruntime.so"),
    };
    let quality = models.join("dpdfnet8_48khz_hr.onnx");
    let light = models.join("dpdfnet2_48khz_hr.onnx");

    // the microphone, started muted
    let mic_conf = root.join("filter-chain.conf");
    let mut mic = spawn_chain(
        &paths,
        &SpawnSpec {
            chain: Chain::Mic,
            conf: &mic_conf,
            model: &quality,
            model_id: "dpdfnet8_48khz_hr",
            inference: Inference::Auto,
            mode: RunMode::Mute.control_value() as u32,
            attn_db: 24.0,
            // "Stay on my voice" is on
            env: hushmic::controller::voice_hold_env(true),
        },
    )
    .expect("the mic chain starts");
    assert_eq!(RunMode::Mute.control_value(), 2, "Mute's control word");
    assert_eq!(mic.generation, engine_generation_of(Chain::Mic));
    let env = env_of(&mic_conf);
    let meter = mic.meter.clone().expect("a meter file");
    let control = mic.control.as_ref().expect("a control file").path.clone();
    let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name(&meter), format!("meter-{}", mic.generation));
    assert_eq!(name(&control), format!("control-{}", mic.generation));
    assert_eq!(
        env.get(hushmic_shm::METER_ENV),
        Some(&meter.display().to_string())
    );
    assert_eq!(
        env.get(hushmic_shm::CONTROL_ENV),
        Some(&control.display().to_string())
    );
    assert_eq!(
        env.get("HUSHMIC_MODEL_PATH"),
        Some(&quality.display().to_string())
    );
    assert_eq!(env.get("HUSHMIC_VOICE_HOLD").map(String::as_str), Some("1"));
    assert_eq!(
        env.get("ORT_DYLIB_PATH"),
        Some(&paths.dylib.display().to_string())
    );
    assert_eq!(
        env.get("HUSHMIC_FALLBACK_MODEL_PATH"),
        Some(&light.display().to_string()),
        "the quality model falls back to the light one"
    );
    assert!(!env.contains_key("HUSHMIC_WORKER_RT_BELOW"), "{env:?}");
    // muted from its first sample: the plugin reads 2, never 0
    assert_eq!(mode_in(&control), (Some(2), Some(24.0)));

    // the speakers cleaner: its own names, generation and environment
    let spk_conf = root.join("filter-chain-speakers.conf");
    let mut spk = spawn_chain(
        &paths,
        &SpawnSpec {
            chain: Chain::Speakers,
            conf: &spk_conf,
            model: &light,
            model_id: "dpdfnet2_48khz_hr",
            inference: Inference::Onnx,
            mode: 0,
            attn_db: 12.0,
            env: &[("HUSHMIC_WORKER_RT_BELOW", "1")],
        },
    )
    .expect("the speakers chain starts");
    assert_eq!(spk.generation, engine_generation_of(Chain::Speakers));
    let env = env_of(&spk_conf);
    let meter = spk.meter.clone().expect("a meter file");
    let control = spk.control.as_ref().expect("a control file").path.clone();
    assert_eq!(name(&meter), format!("speakers-meter-{}", spk.generation));
    assert_eq!(
        name(&control),
        format!("speakers-control-{}", spk.generation)
    );
    assert_eq!(
        env.get("HUSHMIC_WORKER_RT_BELOW").map(String::as_str),
        Some("1")
    );
    assert!(
        !env.contains_key("HUSHMIC_VOICE_HOLD"),
        "the microphone's alone"
    );
    assert_eq!(
        env.get("HUSHMIC_INFERENCE").map(String::as_str),
        Some("onnx")
    );
    // a chain on the light model has no fallback model
    assert!(!env.contains_key("HUSHMIC_FALLBACK_MODEL_PATH"), "{env:?}");
    assert_eq!(mode_in(&control), (Some(0), Some(12.0)));

    // each chain's stderr lands in its own log
    let logs = state.join("hushmic");
    let has = |file: &str, conf: &Path| {
        std::fs::read_to_string(logs.join(file))
            .unwrap_or_default()
            .contains(&format!("stand-in host for {}", conf.display()))
    };
    wait_for("the mic's log line", || has("filter-chain.log", &mic_conf));
    wait_for("the speakers log line", || {
        has("filter-chain-speakers.log", &spk_conf)
    });
    assert!(!has("filter-chain.log", &spk_conf));
    assert!(!has("filter-chain-speakers.log", &mic_conf));

    for c in [&mut mic, &mut spk] {
        let _ = c.child.kill();
        let _ = c.child.wait();
    }
    let _ = std::fs::remove_dir_all(&root);
}
