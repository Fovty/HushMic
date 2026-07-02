#!/usr/bin/env bash
# Fail if a built binary requires a glibc newer than the floor (default 2.35 =
# ubuntu:22.04). Artifacts inherit the BUILD host's glibc requirement: v0.1.2
# was built on ubuntu-24.04 (glibc 2.39) and every artifact — tarball, .deb,
# even the "self-contained" AppImage — refused to run on Ubuntu 22.04 /
# Debian 12 / RHEL 9. Both workflows run this after building.
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
exit "$fail"
