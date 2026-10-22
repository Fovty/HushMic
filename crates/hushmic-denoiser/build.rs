fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "native")]
    native::build();
}

/// The native inference engine (feature `native`): compile the vendored
/// DPDFNet optimized runtime (native/, see its README) into one static
/// archive.
///
/// Flags follow upstream's CMakeLists.txt (Release, no PGO): gnu11, -O3,
/// NDEBUG, no FMA contraction, -mavx2 -mfma (+ -mf16c, + -mavxvnni) only on
/// the files that need them, every model's backend compiled once with its
/// private symbol namespace. Never -march=native or -ffast-math. The
/// AVX-VNNI kernels are built when the compiler and assembler accept them
/// (upstream's probe) and picked at model creation on CPUs that have it;
/// otherwise the AVX2 kernels run. The runtime is x86-64 only: elsewhere
/// nothing is compiled and the engine reports itself unavailable, so the
/// automatic engine choice stays on ONNX.
#[cfg(feature = "native")]
mod native {
    use std::path::{Path, PathBuf};

    /// Every model compiled in: upstream's model ID and its name.
    const MODELS: [(u32, &str); 2] = [(8, "dpdfnet8_48khz_hr"), (2, "dpdfnet2_48khz_hr")];

    /// Options a packager's CFLAGS must not bring in; cc appends CFLAGS
    /// last, so they are removed from the environment cc reads instead.
    /// Optimization level and FP semantics would change the arithmetic or
    /// undercut -O3. LTO would make GCC emit bitcode-only objects that the
    /// Rust linker cannot read: the plugin then links with the runtime's
    /// functions undefined and fails to load at all (Arch's makepkg default).
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

    /// Flags every C and assembly file gets.
    fn base(root: &Path) -> cc::Build {
        let mut b = cc::Build::new();
        b.opt_level(3)
            .define("NDEBUG", None)
            .include(root.join("include"))
            .include(root.join("src"))
            .include(root.join("src/private"))
            .flag("-fno-lto")
            .warnings(true)
            .extra_warnings(true);
        b
    }

    /// `base` plus what upstream applies to C only.
    fn base_c(root: &Path) -> cc::Build {
        let mut b = base(root);
        b.std("gnu11")
            .flag("-ffp-contract=off")
            .flag("-fvisibility=hidden");
        b
    }

    /// Upstream's AVX-VNNI probe: GCC must accept the VEX-encoded
    /// intrinsic and the CPU feature query, and the assembler must encode
    /// the instruction (the operand is not a constant, so it is emitted).
    /// `Err` carries cc's error, for the warning when the probe fails.
    fn compiler_has_avx_vnni(out: &Path) -> Result<(), String> {
        let probe = out.join("avx_vnni_probe.c");
        std::fs::write(
            &probe,
            "#include <immintrin.h>\n\
             int main(void) { __m256i x = _mm256_set1_epi32(__builtin_cpu_supports(\"avxvnni\"));\n\
             x = _mm256_dpbusd_avx_epi32(x, x, x);\n\
             return __builtin_cpu_supports(\"avxvnni\") + _mm256_extract_epi32(x, 0); }\n",
        )
        .unwrap();
        let mut build = cc::Build::new();
        build
            .file(&probe)
            .opt_level(0)
            .flag("-mavx2")
            .flag("-mfma")
            .flag("-mavxvnni")
            .cargo_warnings(false);
        let Err(e) = build.try_compile_intermediates() else {
            return Ok(());
        };
        // cc's error names the command, not why it failed: run the same
        // compile once more to get the compiler's own message.
        let why = build.try_get_compiler().ok().and_then(|tool| {
            let out = tool
                .to_command()
                .arg("-c")
                .arg(&probe)
                .arg("-o")
                .arg(out.join("avx_vnni_probe_retry.o"))
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&out.stderr).into_owned();
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            let errors: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|l| l.contains("error"))
                .collect();
            let pick = if errors.is_empty() { &lines } else { &errors };
            Some(pick.iter().take(3).copied().collect::<Vec<_>>().join(" | "))
        });
        Err(match why {
            Some(w) if !w.is_empty() => w,
            _ => e.to_string().replace('\n', " "),
        })
    }

    /// A model's manifest field `"key": "value"`.
    fn manifest_field(manifest: &str, key: &str) -> Option<String> {
        let at = manifest.find(&format!("\"{key}\""))?;
        let rest = &manifest[at + key.len() + 2..];
        let rest = &rest[rest.find('"')? + 1..];
        Some(rest[..rest.find('"')?].to_string())
    }

    pub fn build() {
        let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("native");
        println!("cargo:rerun-if-changed={}", root.display());

        // The ONNX export each model's runtime was made from: how the Rust
        // side recognizes a model file it can run natively.
        for (_, m) in MODELS {
            let path = root.join("models").join(m).join("manifest.json");
            let manifest = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let sha = manifest_field(&manifest, "source_onnx_sha256")
                .filter(|s| s.len() == 64)
                .unwrap_or_else(|| panic!("{}: no source_onnx_sha256", path.display()));
            println!("cargo:rustc-env=HUSHMIC_NATIVE_ONNX_SHA256_{m}={sha}");
        }

        println!("cargo::rustc-check-cfg=cfg(hushmic_native_runtime)");
        if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64")
            || std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        {
            return;
        }
        sanitize_cflags();
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let probe = compiler_has_avx_vnni(&out);
        let vnni = probe.is_ok();
        if cc::Build::new()
            .try_get_compiler()
            .is_ok_and(|c| c.is_like_clang())
        {
            println!("cargo:warning=building the native engine with clang: upstream validates GCC only, so its numerics are untested with this compiler");
        }
        println!("cargo:rustc-cfg=hushmic_native_runtime");

        let mut objects = Vec::new();
        let mut compile = |mut b: cc::Build, files: Vec<PathBuf>| {
            for f in &files {
                assert!(f.is_file(), "{} missing", f.display());
                b.file(f);
            }
            objects.extend(b.compile_intermediates());
        };
        let common = |names: &[&str]| -> Vec<PathBuf> {
            names
                .iter()
                .map(|n| root.join("src/common").join(n))
                .collect()
        };

        // One backend per model: the shared sources again under the
        // model's symbol namespace, plus its generated graph and kernels.
        for (size, m) in MODELS {
            let model_dir = root.join("src/models").join(m);
            let namespace = root.join(format!("src/private/namespace{size}.h"));
            // cc names objects after their source files, and the shared
            // sources build once per model: one object directory per model.
            let objdir = out.join(m);
            let backend = |mut b: cc::Build, flags: &[&str]| {
                b.out_dir(&objdir)
                    .define("DPDF_MODEL_SIZE", size.to_string().as_str())
                    .define("DPDF_X86_DISPATCH", None)
                    .define("DPDF_QASM64", None)
                    .define("DPDF_QFUSED64", None)
                    .define("DPDF_QPAIR_FIXED", None)
                    .flag("-include")
                    .flag(namespace.to_str().unwrap());
                if size == 2 {
                    b.define("DPDF_QFUSED256", None)
                        .define("DPDF_QPAIR_FUSED64", None);
                }
                if vnni {
                    b.define("DPDF_HAS_AVX_VNNI", None);
                }
                for f in flags {
                    b.flag(f);
                }
                b
            };

            let mut plain = common(&["full_ops.c", "dpdf_dprnn.c", "extended_ops.c"]);
            plain.push(root.join("src/model_backend.c"));
            plain.push(model_dir.join("generated_model.c"));
            compile(backend(base_c(&root), &[]), plain);
            compile(
                backend(base_c(&root), &["-mavx2", "-mfma"]),
                common(&["avx2.c", "int8.c"]),
            );
            compile(
                backend(base_c(&root), &["-mavx2", "-mfma", "-mf16c"]),
                common(&["fp16.c"]),
            );
            if vnni {
                compile(
                    backend(base_c(&root), &["-mavx2", "-mfma", "-mavxvnni"]),
                    common(&["int8_vnni.c"]),
                );
            }

            let mut kernels = vec!["qaffine64_fused"];
            if size == 2 {
                kernels.extend(["qaffine256_fused", "qaffine4pair64_fused"]);
            }
            let mut isas = vec!["avx2"];
            if vnni {
                isas.push("vnni");
            }
            let mut asm = Vec::new();
            for isa in isas {
                asm.extend(common(&[
                    &format!("qdot64_fixed_{isa}.S"),
                    &format!("qdot4pair_fixed_{isa}.S"),
                ]));
                for k in &kernels {
                    asm.push(model_dir.join(format!("{k}_{isa}.S")));
                }
            }
            compile(backend(base(&root), &[]), asm);
        }
        compile(base_c(&root), vec![root.join("src/runtime.c")]);

        cc::Build::new()
            .objects(&objects)
            .compile("hushmic_dpdf_native");
        println!("cargo:rustc-link-lib=m");
        if let Err(e) = probe {
            println!("cargo:warning=AVX-VNNI kernels not built (compiler or assembler lacks AVX-VNNI); the AVX2 kernels run on every CPU; probe: {e}");
        }
    }
}
