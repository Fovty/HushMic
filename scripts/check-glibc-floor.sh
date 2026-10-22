#!/usr/bin/env bash
# Fail if a built binary requires a glibc newer than the floor (default 2.35 =
# ubuntu:22.04). Artifacts inherit the BUILD host's glibc requirement: built on
# a newer base, every artifact — tarball, .deb, even the "self-contained"
# AppImage — refuses to run on Ubuntu 22.04 / Debian 12 / RHEL 9. Both
# workflows run this after building. It also checks that the plugin loads
# and carries the native engine's AVX-VNNI kernels.
set -euo pipefail

FLOOR="${1:-2.35}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fail=0
for f in "$REPO_ROOT/target/release/hushmic" \
         "$REPO_ROOT/target/release/libdpdfnet_ladspa.so"; do
  if [ ! -f "$f" ]; then
    echo "error: $f not built" >&2
    exit 1
  fi
  max="$(objdump -T "$f" | grep -oE 'GLIBC_[0-9.]+' | sed 's/GLIBC_//' | sort -Vu | tail -1)"
  echo "$(basename "$f"): max required glibc symbol version = $max (floor $FLOOR)"
  highest="$(printf '%s\n%s\n' "$max" "$FLOOR" | sort -V | tail -1)"
  if [ "$highest" != "$FLOOR" ]; then
    echo "error: $(basename "$f") requires glibc $max > $FLOOR — the build host is too new;" >&2
    echo "       release builds must run in the ubuntu:22.04 container (see release.yml)." >&2
    fail=1
  fi
done
# The same builds must load: imports only from glibc/libgcc, LADSPA-only
# exports (the native engine's C linked in, not left undefined).
bash "$REPO_ROOT/scripts/check-plugin-symbols.sh" "$REPO_ROOT/target/release/libdpdfnet_ladspa.so" || fail=1
# Release plugins carry the native engine's AVX-VNNI kernels. build.rs
# leaves them out (with a warning) when the compiler or assembler cannot
# build them; a release must not ship AVX2-only by accident.
vnni="$(objdump -d --no-show-raw-insn "$REPO_ROOT/target/release/libdpdfnet_ladspa.so" | grep -c vpdpbusd || true)"
if [ "$vnni" -gt 0 ]; then
  echo "libdpdfnet_ladspa.so: AVX-VNNI kernels present ($vnni vpdpbusd)"
else
  echo "error: libdpdfnet_ladspa.so has no AVX-VNNI kernels (no vpdpbusd); the toolchain could not build them" >&2
  fail=1
fi
exit "$fail"
