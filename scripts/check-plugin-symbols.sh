#!/usr/bin/env bash
# Fail if the LADSPA plugin would not load or exports more than LADSPA
# needs. Every symbol it imports must come from glibc (libc, libm, ld.so)
# or libgcc, i.e. carry a GLIBC_/GCC_ version: an unversioned undefined
# symbol means something did not link in, e.g. the native engine's C
# compiled to LTO bitcode that the Rust linker ignored. dlopen with
# RTLD_NOW then fails and PipeWire cannot load the plugin at all.
#
#   scripts/check-plugin-symbols.sh [path/to/libdpdfnet_ladspa.so]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SO="${1:-${CARGO_TARGET_DIR:-$REPO_ROOT/target}/release/libdpdfnet_ladspa.so}"
[ -f "$SO" ] || { echo "error: $SO not built" >&2; exit 1; }

# Weak references the toolchain's own startup code leaves unversioned.
ALLOWED_WEAK='^(__gmon_start__|_ITM_(de)?registerTMCloneTable|__cxa_finalize)$'

fail=0
# nm prints symbol versions (name@GLIBC_2.2.5) only since binutils 2.35.
# The plugin always imports versioned glibc symbols, so no '@' at all means
# an older nm; objdump -T has shown the version column for much longer.
if nm -D --undefined-only "$SO" | grep -q '@'; then
  bad_imports="$(nm -D --undefined-only "$SO" | awk -v weak="$ALLOWED_WEAK" '
    $1 == "U" && $2 !~ /@(GLIBC|GCC)_/ { print $2 }
    $1 == "w" && $2 !~ /@(GLIBC|GCC)_/ && $2 !~ weak { print $2 }')"
elif command -v objdump >/dev/null 2>&1; then
  # "0000000000000000  w   DF *UND*  0000000000000000 (GLIBC_2.2.5) name";
  # older objdump prints the version without the parentheses.
  bad_imports="$(objdump -T "$SO" | awk -v weak="$ALLOWED_WEAK" '
    /\*UND\*/ && !/[ (](GLIBC|GCC)_[0-9.]+/ && !($2 == "w" && $NF ~ weak) { print $NF }')"
else
  echo "error: this nm does not print symbol versions (binutils < 2.35) and objdump is missing;" >&2
  echo "       cannot tell glibc imports from unresolved ones. Install binutils >= 2.35." >&2
  exit 2
fi
if [ -n "$bad_imports" ]; then
  echo "error: $(basename "$SO") imports symbols no system library provides:" >&2
  echo "$bad_imports" | sed 's/^/  /' >&2
  fail=1
fi
exports="$(nm -D --defined-only "$SO" | awk '$2 ~ /^[TDBVW]$/ { print $3 }' | sort | tr '\n' ' ')"
if [ "$exports" != "get_ladspa_descriptor ladspa_descriptor " ]; then
  echo "error: $(basename "$SO") exports more than the LADSPA entry points: $exports" >&2
  fail=1
fi
[ "$fail" -eq 0 ] && echo "$(basename "$SO"): imports only glibc/libgcc, exports only the LADSPA entry points"
exit "$fail"
