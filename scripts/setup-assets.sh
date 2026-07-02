#!/usr/bin/env bash
# Provision the gitignored binary assets that the build needs:
#   1. ONNX models   -> assets/models/{dpdfnet8_48khz_hr,dpdfnet2_48khz_hr}.onnx
#   2. ONNX Runtime  -> assets/lib/libonnxruntime.so{,.1,.1.27.0}
#
# Self-sufficient for a fresh CI checkout, and SUPPLY-CHAIN PINNED: the dpdfnet
# package version, every model file, and the ONNX Runtime tarball are verified
# against the sha256 pins below, so release artifacts cannot silently drift
# between builds of the same tag. Idempotent — re-running verifies what is
# already in place (a truncated/corrupted asset is re-fetched, not trusted).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# dpdfnet 0.5.1 declares Requires-Python >=3.11; on hosts whose default
# python3 is older (ubuntu:22.04 = 3.10) point PYTHON at a newer interpreter
# (the CI containers install python3.11 and set this).
PYTHON="${PYTHON:-python3}"
ORT_VERSION="1.27.0"
ORT_TGZ="onnxruntime-linux-x64-${ORT_VERSION}.tgz"
ORT_URL="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${ORT_TGZ}"
ORT_TGZ_SHA256="547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f"
ORT_SO_SHA256="4061866361d9a8d2872f5f419c5515ce35a830a0c5c77ce1723320ac0dbabfc7"

DPDFNET_VERSION="0.5.1"
MODELS=(dpdfnet8_48khz_hr dpdfnet2_48khz_hr)
declare -A MODEL_SHA256=(
  [dpdfnet8_48khz_hr]="7b3afbb260a08fe9af3d16e3bda992971be1e7e951d1dee7c2d235f5c43f5631"
  [dpdfnet2_48khz_hr]="7f0575a5cec0ba4ffd8f8bd657e06d007e4ccdd955d76faab922b9d3291dc14b"
)

mkdir -p "$REPO_ROOT/assets/lib" "$REPO_ROOT/assets/models"

# file_ok <path> <sha256>: present AND hash-verified (catches truncation).
file_ok() {
  [ -f "$1" ] || return 1
  echo "$2  $1" | sha256sum -c - >/dev/null 2>&1
}

# atomic_install <src> <dest> <mode>: no partially-copied file can ever sit at
# the final path (a torn copy would pass a bare -f presence check forever).
atomic_install() {
  install -m "$3" "$1" "$2.tmp"
  mv -f "$2.tmp" "$2"
}

# ---------------------------------------------------------------------------
# 1. ONNX models: fetched via the PINNED dpdfnet package, then hash-verified.
# ---------------------------------------------------------------------------
models_present() {
  local m
  for m in "${MODELS[@]}"; do
    file_ok "$REPO_ROOT/assets/models/$m.onnx" "${MODEL_SHA256[$m]}" || return 1
  done
  return 0
}

fetch_models() {
  echo "Models missing or failed verification; fetching via dpdfnet==${DPDFNET_VERSION}..."
  # Fail with a clear pointer instead of pip's cryptic 'No matching
  # distribution' when the interpreter is too old (Ubuntu 22.04 = 3.10).
  if ! "$PYTHON" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' 2>/dev/null; then
    echo "ERROR: dpdfnet==${DPDFNET_VERSION} needs Python >= 3.11, but '$PYTHON' is $("$PYTHON" -V 2>&1 || echo 'not runnable')." >&2
    echo "       Point PYTHON at a newer interpreter, e.g.: PYTHON=python3.11 $0" >&2
    exit 1
  fi
  local cache="${HOME}/.cache/dpdfnet/models"
  local dpdfnet_bin=""

  # Prefer an isolated venv (reproducible, no system pollution); fall back to a
  # user/system pip install if venv creation is unavailable in this environment.
  local venv="$REPO_ROOT/.venv-assets"
  if "$PYTHON" -m venv "$venv" >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    "$venv/bin/pip" install --quiet --upgrade pip
    "$venv/bin/pip" install --quiet "dpdfnet==${DPDFNET_VERSION}"
    dpdfnet_bin="$venv/bin/dpdfnet"
  else
    echo "venv unavailable; installing dpdfnet with pip directly" >&2
    if ! "$PYTHON" -m pip install --quiet --user "dpdfnet==${DPDFNET_VERSION}" 2>/dev/null; then
      "$PYTHON" -m pip install --quiet --break-system-packages "dpdfnet==${DPDFNET_VERSION}"
    fi
    dpdfnet_bin="$(command -v dpdfnet || true)"
    [ -n "$dpdfnet_bin" ] || dpdfnet_bin="$PYTHON -m dpdfnet"
  fi

  echo "Downloading the required models..."
  local dl
  for dl in "${MODELS[@]}"; do
    # shellcheck disable=SC2086
    $dpdfnet_bin download "$dl"
  done

  local m
  for m in "${MODELS[@]}"; do
    if [ ! -f "$cache/$m.onnx" ]; then
      echo "ERROR: expected '$cache/$m.onnx' after 'dpdfnet download' but it is missing." >&2
      exit 1
    fi
    if ! file_ok "$cache/$m.onnx" "${MODEL_SHA256[$m]}"; then
      echo "ERROR: $m.onnx does not match its pinned sha256 — upstream content changed" >&2
      echo "       or the download is corrupted. Refusing to use it; if upstream" >&2
      echo "       legitimately re-released the model, update MODEL_SHA256 here." >&2
      exit 1
    fi
    atomic_install "$cache/$m.onnx" "$REPO_ROOT/assets/models/$m.onnx" 644
    echo "  copied + verified $m.onnx"
  done
}

if models_present; then
  echo "Models already present and verified; skipping fetch."
else
  fetch_models
fi

# ---------------------------------------------------------------------------
# 2. ONNX Runtime shared library (pinned ${ORT_VERSION}) for ort's load-dynamic.
#    We keep ONE real file (libonnxruntime.so.1.27.0) plus the two symlinks
#    (.so.1 -> .so.1.27.0, .so -> .so.1) that ort/loaders expect.
# ---------------------------------------------------------------------------
ORT_REAL="libonnxruntime.so.${ORT_VERSION}"
if ! file_ok "$REPO_ROOT/assets/lib/$ORT_REAL" "$ORT_SO_SHA256"; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  echo "Downloading ONNX Runtime ${ORT_VERSION}..."
  curl -fsSL "$ORT_URL" -o "$tmp/$ORT_TGZ"
  if ! file_ok "$tmp/$ORT_TGZ" "$ORT_TGZ_SHA256"; then
    echo "ERROR: $ORT_TGZ does not match its pinned sha256; aborting." >&2
    exit 1
  fi
  tar -xzf "$tmp/$ORT_TGZ" -C "$tmp"
  # The tarball ships libonnxruntime.so.<ver> (and sometimes symlinks); copy the
  # real versioned object only.
  src="$(find "$tmp/onnxruntime-linux-x64-${ORT_VERSION}/lib" -maxdepth 1 -type f -name "libonnxruntime.so.*" | head -1)"
  if [ -z "$src" ]; then
    echo "ERROR: libonnxruntime.so.<version> not found in the downloaded archive." >&2
    exit 1
  fi
  atomic_install "$src" "$REPO_ROOT/assets/lib/$ORT_REAL" 755
  # Catch an inconsistent pin pair immediately: if ORT_TGZ_SHA256 was bumped
  # without ORT_SO_SHA256, the presence check above would fail on EVERY future
  # run and silently re-download the tarball forever instead of erroring once.
  if ! file_ok "$REPO_ROOT/assets/lib/$ORT_REAL" "$ORT_SO_SHA256"; then
    echo "ERROR: extracted $ORT_REAL does not match ORT_SO_SHA256 — the tarball and" >&2
    echo "       .so pins are inconsistent; update both together." >&2
    exit 1
  fi
  rm -rf "$tmp"
  trap - EXIT
fi

# Normalize the symlink chain (idempotent).
( cd "$REPO_ROOT/assets/lib"
  ln -sf "$ORT_REAL" "libonnxruntime.so.1"
  ln -sf "libonnxruntime.so.1" "libonnxruntime.so"
)

echo "Assets ready:"
ls -lh "$REPO_ROOT/assets/models/"
ls -lh "$REPO_ROOT/assets/lib/"
