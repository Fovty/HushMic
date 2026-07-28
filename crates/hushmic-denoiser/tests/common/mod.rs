//! Shared helpers: locate the repo's development assets (provisioned by
//! scripts/setup-assets.sh, absent on bare checkouts) and bring up the
//! bundled ONNX Runtime. Tests self-skip when assets are missing.
#![allow(dead_code)] // each test binary uses a subset

use hushmic_denoiser::Denoiser;
use std::path::PathBuf;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// CI exports HUSHMIC_ASSERT_ASSETS=1 so a provisioning regression fails the
/// suite loudly instead of turning every asset-gated test into a silent skip.
fn assert_assets() -> bool {
    std::env::var("HUSHMIC_ASSERT_ASSETS").as_deref() == Ok("1")
}

pub fn model_path(name: &str) -> Option<PathBuf> {
    let p = repo_root().join("assets/models").join(name);
    if !p.exists() && assert_assets() {
        panic!(
            "{} missing but HUSHMIC_ASSERT_ASSETS=1 — assets must be provisioned",
            p.display()
        );
    }
    p.exists().then_some(p)
}

/// Commit the repo's bundled runtime (AlreadyInitialized is fine — another
/// test in this binary may have won). None = assets absent, caller skips.
pub fn init_dev_runtime() -> Option<()> {
    let rt = repo_root().join("assets/lib/libonnxruntime.so");
    if !rt.exists() {
        if assert_assets() {
            panic!(
                "{} missing but HUSHMIC_ASSERT_ASSETS=1 — assets must be provisioned",
                rt.display()
            );
        }
        return None;
    }
    hushmic_denoiser::init_runtime(rt).expect("bundled runtime must load");
    Some(())
}

/// A ready `Denoiser` on the named dev model, or None to skip.
pub fn dev_denoiser(model: &str) -> Option<Denoiser> {
    let mp = model_path(model)?;
    init_dev_runtime()?;
    Some(Denoiser::from_file(mp).expect("denoiser"))
}
