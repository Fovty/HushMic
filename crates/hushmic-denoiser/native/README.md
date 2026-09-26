# Vendored DPDFNet native runtime

Unmodified copies of the DPDFNet authors' standalone C inference runtime and
the generated model code for `dpdfnet8_48khz_hr` and `dpdfnet2_48khz_hr`,
taken from <https://github.com/ceva-ip/DPDFNet> at commit
`6a5dbd3ea5dbea88c30a2be8e7c689b5eb26b533`:

| here | upstream |
| --- | --- |
| `runtime/` | `native_inference/native/` (only the files the integration target compiles, plus their headers) |
| `artifacts/v1/` | `native_inference/artifacts/v1/` without the `weights.f32` blobs |

`artifacts/v1/SHA256SUMS` is upstream's checksum list; the crate's test
suite checks every vendored file it names against it. The weight blobs are
not in this repository: `scripts/setup-assets.sh` downloads them from the
same commit and verifies the same checksums, and the runtime verifies them
again before loading.

`build.rs` compiles these files with the flags of upstream's
`native_inference/native/integration/CMakeLists.txt`. Only the versioned C
API (`<model>_get_api(1)`, see `runtime/native_api.h`) is used from Rust.

To update: copy the same file set from a newer upstream commit, keep the
files unmodified, and update the commit here, the checksums in
`scripts/setup-assets.sh` and the weight URLs there.

Licensed under the Apache License, Version 2.0 (`LICENSE`, upstream's
license file).
