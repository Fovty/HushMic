//! Shared helpers: locate the repo's development assets (provisioned by
//! scripts/setup-assets.sh, absent on bare checkouts). Tests self-skip
//! when assets are missing; CI exports HUSHMIC_ASSERT_ASSETS=1 so a
//! provisioning regression fails loudly instead of silently skipping.
#![allow(dead_code)]

use std::path::PathBuf;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn assert_assets() -> bool {
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

pub fn runtime_path() -> Option<PathBuf> {
    let p = std::env::var_os("ORT_DYLIB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("assets/lib/libonnxruntime.so"));
    if !p.exists() && assert_assets() {
        panic!(
            "{} missing but HUSHMIC_ASSERT_ASSETS=1 — assets must be provisioned",
            p.display()
        );
    }
    p.exists().then_some(p)
}
