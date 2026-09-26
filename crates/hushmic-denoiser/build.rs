fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "native")]
    native::build();
}

/// The native inference engine (feature `native`): compile the vendored
/// DPDFNet C runtime (native/, see its README) into one static archive.
///
/// Flags follow upstream's integration CMake target (Release): gnu11, -O3,
/// NDEBUG, no FMA contraction, runtime AVX2/FMA dispatch on x86_64 with
/// -mavx2 -mfma (+ -mf16c) only on the files that need them. Never
/// -march=native or -ffast-math. Other architectures build the portable
/// scalar path only; there the INT8 preset reports unsupported and the
/// automatic engine choice stays on ONNX.
#[cfg(feature = "native")]
mod native {
    use std::path::{Path, PathBuf};

    /// Every model compiled in, by the symbol prefix of its generated code.
    const MODELS: [&str; 2] = ["dpdfnet8_48khz_hr", "dpdfnet2_48khz_hr"];

    /// Options a packager's CFLAGS must not bring in; cc appends CFLAGS
    /// last, so they are removed from the environment cc reads instead.
    /// Optimization level and FP semantics would change the arithmetic or
    /// undercut -O3. LTO would make GCC emit bitcode-only objects that the
    /// Rust linker cannot read: the plugin then links with the model
    /// getters undefined and fails to load at all (Arch's makepkg default).
    fn unwanted_flag(f: &str) -> bool {
        (f.starts_with("-O") && f.len() <= 6)
            || f.starts_with("-ffp-contract=")
            || f.starts_with("-flto")
            || matches!(
                f,
                "-ffast-math"
                    | "-funsafe-math-optimizations"
                    | "-fassociative-math"
                    | "-freciprocal-math"
                    | "-ffinite-math-only"
                    | "-ffat-lto-objects"
                    | "-fno-fat-lto-objects"
            )
    }

    fn sanitize_cflags() {
        let target = std::env::var("TARGET").unwrap_or_default();
        for var in [
            format!("CFLAGS_{target}"),
            format!("CFLAGS_{}", target.replace(['-', '.'], "_")),
            "TARGET_CFLAGS".to_string(),
            "HOST_CFLAGS".to_string(),
            "CFLAGS".to_string(),
        ] {
            println!("cargo:rerun-if-env-changed={var}");
            if let Ok(v) = std::env::var(&var) {
                let kept: Vec<&str> = v.split_whitespace().filter(|f| !unwanted_flag(f)).collect();
                std::env::set_var(&var, kept.join(" "));
            }
        }
    }

    fn base(runtime: &Path, x86: bool) -> cc::Build {
        let mut b = cc::Build::new();
        b.std("gnu11")
            .opt_level(3)
            .define("NDEBUG", None)
            .include(runtime)
            .flag("-ffp-contract=off")
            .flag("-fno-lto")
            .warnings(true)
            .extra_warnings(true);
        if x86 {
            b.define("DPDF_X86_DISPATCH", None);
        }
        b
    }

    /// A model's manifest field `"key": "value"`.
    fn manifest_field(manifest: &str, key: &str) -> Option<String> {
        let at = manifest.find(&format!("\"{key}\""))?;
        let rest = &manifest[at + key.len() + 2..];
        let rest = &rest[rest.find('"')? + 1..];
        Some(rest[..rest.find('"')?].to_string())
    }

    pub fn build() {
        sanitize_cflags();
        let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("native");
        let runtime = root.join("runtime");
        let artifacts = root.join("artifacts/v1");
        let x86 = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64");
        println!("cargo:rerun-if-changed={}", root.display());

        let mut objects = Vec::new();
        let mut group = |files: Vec<PathBuf>, flags: &[&str]| {
            let mut b = base(&runtime, x86);
            for f in &files {
                assert!(f.is_file(), "{} missing", f.display());
                b.file(f);
            }
            for f in flags {
                b.flag(f);
            }
            objects.extend(b.compile_intermediates());
        };
        let src = |names: &[&str]| names.iter().map(|n| runtime.join(n)).collect::<Vec<_>>();
        let mut common = src(&["dpdf_dprnn.c", "full_ops.c", "extended_ops.c"]);
        for m in MODELS {
            common.push(artifacts.join(m).join("generated_model.c"));
        }
        group(common, &[]);
        if x86 {
            group(src(&["avx2.c", "int8.c"]), &["-mavx2", "-mfma"]);
            group(src(&["fp16.c"]), &["-mavx2", "-mfma", "-mf16c"]);
        }
        cc::Build::new()
            .objects(&objects)
            .compile("hushmic_dpdf_native");
        println!("cargo:rustc-link-lib=m");

        // The ONNX export each generated model was made from: how the Rust
        // side recognizes a model file it can run natively.
        for m in MODELS {
            let path = artifacts.join(m).join("manifest.json");
            let manifest = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let sha = manifest_field(&manifest, "source_sha256")
                .filter(|s| s.len() == 64)
                .unwrap_or_else(|| panic!("{}: no source_sha256", path.display()));
            println!("cargo:rustc-env=HUSHMIC_NATIVE_ONNX_SHA256_{m}={sha}");
        }
    }
}
