# Vendored DPDFNet native runtime

Unmodified copies of the DPDFNet authors' optimized C runtime for
`dpdfnet8_48khz_hr` and `dpdfnet2_48khz_hr` (W7A8: 7-bit weights, 8-bit
activations, AVX2 assembly kernels with AVX-VNNI variants), taken from
<https://github.com/ceva-ip/DPDFNet> at commit
`afa8292f3b8083c3daadd06dbcfe4ff877dd3172`, directory `optimized_inference/`.
The layout here is the same as there:

| here | upstream `optimized_inference/` |
| --- | --- |
| `include/`, `src/` | the same, complete |
| `models/` | `models/` without the `weights.f32` blobs |
| `CMakeLists.txt` | the same, for reference: build.rs follows its flags |
| `LICENSE` | the same |

Left out: benchmarks, docs, examples, tests, tools and validation data.

`UPSTREAM.sha256` lists every vendored file with its SHA-256, taken from the
upstream checkout (`sha256sum -c UPSTREAM.sha256` there passes); the
crate's test suite checks the files here against it. The weight blobs are
not in this repository: `scripts/setup-assets.sh` downloads them from the
same commit and verifies the checksums in `models/SHA256SUMS`, and the Rust
side verifies them again against the runtime's metadata before loading.
They are byte-identical to the weights of the earlier vendored runtime.

`build.rs` compiles these files with the flags of upstream's
`CMakeLists.txt` (Release, without the optional model-8 compiler profile).
The AVX-VNNI kernels are built when the compiler and assembler support
them, with upstream's probe; the runtime picks them on CPUs that have
AVX-VNNI and the AVX2 kernels elsewhere. From Rust, the public API in
`include/dpdfnet.h` (ABI 1) is used, plus one private symbol per model:
`dpdfnet8_private_has_avx_vnni` and `dpdfnet2_private_has_avx_vnni`
(`dpdf_has_avx_vnni` under the model's namespace, see
`src/private/namespace*.h`), the predicate matrix creation uses to pick the
kernels, so the engine can say which set runs. A rename upstream fails the
link, never silently.

Upstream validates GCC only; build.rs warns when the compiler is clang. A
packager's `-march`/`-mtune` CFLAGS still reach the C files that are not
dispatched (they are not filtered); with `-ffp-contract=off` they cannot
fuse operations, so the numerics stay the same.

The output bits depend on the C library's libm (the runtime calls `expf`,
`tanhf` and friends): glibc 2.44 differs from 2.35/2.39 on the same binary
and CPU, while the AVX-VNNI and AVX2 kernels match bit for bit. The crate's
golden-output test therefore compares exactly only on recorded glibc
versions.

To update: copy the same file set from a newer upstream commit, keep the
files unmodified, regenerate `UPSTREAM.sha256` (`find . -type f ! -name
README.md ! -name UPSTREAM.sha256 | sed 's|^\./||' | sort | xargs
sha256sum`), check build.rs against the new `CMakeLists.txt`, and update
the commit here plus the checksums and weight URLs in
`scripts/setup-assets.sh`.

Licensed under the Apache License, Version 2.0 (`LICENSE`, upstream's
license file).
