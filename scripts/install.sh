#!/bin/sh
# hushmic installer — portable POSIX sh.
#
# Two modes, auto-detected:
#   * Co-located: when this script sits inside an extracted release tarball
#     (next to bin/hushmic, lib/, share/), it installs those files.
#   * Standalone: otherwise it downloads the latest release tarball from GitHub,
#     extracts it, and installs from there.
#
# Usage:
#   install.sh [--prefix DIR]   install (default prefix /usr; needs root)
#   install.sh --uninstall [--prefix DIR]
#   install.sh -h | --help
set -eu

REPO="Fovty/hushmic"
ARCH="x86_64"
# VERSION is derived later from the actual payload (the co-located dir name or
# the latest release tarball), never hardcoded — so this script does not go
# stale when the released version changes.
VERSION=""

PREFIX="/usr"
ACTION="install"

# ---------------------------------------------------------------------------
# Arg parsing
# ---------------------------------------------------------------------------
usage() {
  cat <<EOF
hushmic installer

Usage:
  $0 [--prefix DIR]          Install (default --prefix /usr, requires root)
  $0 --uninstall [--prefix DIR]
  $0 -h | --help

Examples:
  sudo $0                     # system install under /usr
  $0 --prefix "\$HOME/.local"  # user-local install (no root needed)
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs an argument}"; shift 2 ;;
    --prefix=*) PREFIX="${1#*=}"; shift ;;
    --uninstall) ACTION="uninstall"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# Normalize PREFIX to an absolute path (best-effort).
case "$PREFIX" in
  /*) : ;;
  *) PREFIX="$(pwd)/$PREFIX" ;;
esac

# ---------------------------------------------------------------------------
# Privilege handling
# ---------------------------------------------------------------------------
NEED_SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  # A system prefix (anything outside $HOME) needs root even when the prefix
  # dir itself looks writable: some systems have an oddly user-owned /usr while
  # its standard subdirs (lib/ladspa, share/...) stay root-owned, which would
  # otherwise leave a half-written install. Prefixes under $HOME only need sudo
  # if they are not actually writable.
  case "$PREFIX/" in
    "$HOME"/*) _system_prefix=0 ;;
    *)         _system_prefix=1 ;;
  esac
  if [ "$_system_prefix" = 1 ] || ! mkdir -p "$PREFIX" 2>/dev/null || [ ! -w "$PREFIX" ]; then
    if command -v sudo >/dev/null 2>&1; then
      NEED_SUDO="yes"
    else
      echo "error: installing to '$PREFIX' requires root, and sudo was not found." >&2
      echo "       Re-run as root (e.g. 'curl ... | sudo sh') or use --prefix \"\$HOME/.local\"." >&2
      exit 1
    fi
  fi
fi

as_root() {
  if [ -n "$NEED_SUDO" ]; then
    sudo "$@"
  else
    "$@"
  fi
}

# ---------------------------------------------------------------------------
# Install-layout destinations (uniform: PREFIX + layout)
# ---------------------------------------------------------------------------
DEST_BIN="$PREFIX/bin"
DEST_LADSPA="$PREFIX/lib/ladspa"
DEST_LIB="$PREFIX/lib/hushmic"
DEST_MODELS="$PREFIX/share/hushmic/models"
DEST_APPS="$PREFIX/share/applications"
DEST_ICONS="$PREFIX/share/icons/hicolor/256x256/apps"
DEST_LICENSES="$PREFIX/share/licenses/hushmic"

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
do_uninstall() {
  echo "Removing hushmic from prefix: $PREFIX"
  as_root rm -f "$DEST_BIN/hushmic" "$DEST_BIN/hushmic-uninstall"
  as_root rm -f "$DEST_LADSPA/libdpdfnet_ladspa.so"
  as_root rm -rf "$DEST_LIB"
  as_root rm -rf "$PREFIX/share/hushmic"
  as_root rm -f "$DEST_APPS/hushmic.desktop"
  as_root rm -f "$DEST_ICONS/hushmic.png"
  as_root rm -rf "$DEST_LICENSES"
  echo "Done. (Per-user config under ~/.config/hushmic was left untouched.)"
}

if [ "$ACTION" = "uninstall" ]; then
  do_uninstall
  exit 0
fi

# ---------------------------------------------------------------------------
# Locate the payload (co-located or downloaded)
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
CLEANUP_DIR=""
GEN_DIR=""
cleanup() {
  [ -n "$CLEANUP_DIR" ] && rm -rf "$CLEANUP_DIR"
  [ -n "$GEN_DIR" ] && rm -rf "$GEN_DIR"
  return 0
}
trap cleanup EXIT INT TERM

if [ -f "$SCRIPT_DIR/bin/hushmic" ]; then
  PAYLOAD="$SCRIPT_DIR"
  echo "Installing from co-located release payload: $PAYLOAD"
else
  echo "No co-located payload; downloading the latest release..."
  if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    echo "error: need curl or wget to download the release." >&2
    exit 1
  fi
  fetch() { if command -v curl >/dev/null 2>&1; then curl -fsSL "$1"; else wget -qO- "$1"; fi; }
  # Resolve the latest release's tarball asset URL from the GitHub API, so the
  # installer is version-independent (works for any future tag, not just 0.1.0).
  LATEST_URL="$(fetch "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep 'browser_download_url' \
    | grep -E "hushmic-[0-9][^\"]*-${ARCH}\.tar\.gz" \
    | head -1 | cut -d'"' -f4)"
  if [ -z "$LATEST_URL" ]; then
    echo "error: could not find a hushmic-*-${ARCH}.tar.gz asset in the latest release." >&2
    exit 1
  fi
  CLEANUP_DIR="$(mktemp -d)"
  tgz="$CLEANUP_DIR/$(basename "$LATEST_URL")"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$LATEST_URL" -o "$tgz"
  else
    wget -qO "$tgz" "$LATEST_URL"
  fi
  tar -xzf "$tgz" -C "$CLEANUP_DIR"
  # The tarball has a single top-level dir: hushmic-<ver>-<arch>/
  PAYLOAD="$(find "$CLEANUP_DIR" -maxdepth 4 -type f -name hushmic -path '*/bin/hushmic' -exec dirname {} \; | head -1)"
  PAYLOAD="${PAYLOAD%/bin}"
  if [ -z "$PAYLOAD" ] || [ ! -f "$PAYLOAD/bin/hushmic" ]; then
    echo "error: could not locate bin/hushmic inside the downloaded tarball." >&2
    exit 1
  fi
  echo "Installing from downloaded payload: $PAYLOAD"
fi

# Version for the summary message, parsed from the payload dir name
# (hushmic-<ver>-<arch>); falls back gracefully if the name is unexpected.
VERSION="$(basename "$PAYLOAD" | sed -n "s/^hushmic-\(.*\)-${ARCH}\$/\1/p")"
[ -n "$VERSION" ] || VERSION="(unknown)"

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------
install_file() {
  # install_file <src> <dest-dir> <mode>
  src="$1"; destdir="$2"; mode="$3"
  as_root mkdir -p "$destdir"
  as_root cp -f "$src" "$destdir/"
  as_root chmod "$mode" "$destdir/$(basename "$src")"
}

echo "Installing to prefix: $PREFIX"
install_file "$PAYLOAD/bin/hushmic" "$DEST_BIN" 755
install_file "$PAYLOAD/lib/ladspa/libdpdfnet_ladspa.so" "$DEST_LADSPA" 644

# ONNX Runtime shared lib(s) — preserve the symlink chain.
as_root mkdir -p "$DEST_LIB"
as_root cp -Pf "$PAYLOAD"/lib/hushmic/libonnxruntime.so* "$DEST_LIB/"

# Models.
as_root mkdir -p "$DEST_MODELS"
for m in "$PAYLOAD"/share/hushmic/models/*.onnx; do
  install_file "$m" "$DEST_MODELS" 644
done

install_file "$PAYLOAD/share/applications/hushmic.desktop" "$DEST_APPS" 644
[ -f "$PAYLOAD/share/icons/hicolor/256x256/apps/hushmic.png" ] && install_file "$PAYLOAD/share/icons/hicolor/256x256/apps/hushmic.png" "$DEST_ICONS" 644
[ -f "$PAYLOAD/LICENSE-MIT" ] && install_file "$PAYLOAD/LICENSE-MIT" "$DEST_LICENSES" 644
[ -f "$PAYLOAD/LICENSE-APACHE" ] && install_file "$PAYLOAD/LICENSE-APACHE" "$DEST_LICENSES" 644

# ---------------------------------------------------------------------------
# Generate the uninstaller (bakes this prefix).
# ---------------------------------------------------------------------------
GEN_DIR="$(mktemp -d)"
uninstaller="$GEN_DIR/hushmic-uninstall"
cat > "$uninstaller" <<EOF
#!/bin/sh
# Auto-generated by hushmic install.sh — removes the install at prefix below.
set -eu
PREFIX="$PREFIX"
SUDO=""
if [ "\$(id -u)" -ne 0 ]; then
  case "\$PREFIX/" in
    "\$HOME"/*) [ -w "\$PREFIX" ] || { command -v sudo >/dev/null 2>&1 && SUDO="sudo"; } ;;
    *)          command -v sudo >/dev/null 2>&1 && SUDO="sudo" ;;
  esac
fi
\$SUDO rm -f "\$PREFIX/bin/hushmic" "\$PREFIX/bin/hushmic-uninstall"
\$SUDO rm -f "\$PREFIX/lib/ladspa/libdpdfnet_ladspa.so"
\$SUDO rm -rf "\$PREFIX/lib/hushmic"
\$SUDO rm -rf "\$PREFIX/share/hushmic"
\$SUDO rm -f "\$PREFIX/share/applications/hushmic.desktop"
\$SUDO rm -f "\$PREFIX/share/icons/hicolor/256x256/apps/hushmic.png"
\$SUDO rm -rf "\$PREFIX/share/licenses/hushmic"
echo "hushmic uninstalled from \$PREFIX (config in ~/.config/hushmic left intact)."
EOF
chmod +x "$uninstaller"
install_file "$uninstaller" "$DEST_BIN" 755

# ---------------------------------------------------------------------------
# Next steps
# ---------------------------------------------------------------------------
echo
echo "hushmic ${VERSION} installed."
echo
echo "Start it (system tray):  hushmic --tray"
echo "Uninstall:               hushmic-uninstall   (or: $0 --uninstall --prefix \"$PREFIX\")"
case "$PREFIX" in
  /usr|/usr/local) : ;;
  *)
    echo
    echo "NOTE: you installed under a non-standard prefix. The binary's compiled-in"
    echo "default paths point at /usr; export these so it finds the bundled assets:"
    echo "  export ORT_DYLIB_PATH=\"$DEST_LIB/libonnxruntime.so\""
    echo "  export HUSHMIC_MODEL_DIR=\"$DEST_MODELS\""
    echo "  export HUSHMIC_PLUGIN_SO=\"$DEST_LADSPA/libdpdfnet_ladspa.so\""
    echo "  export PATH=\"$DEST_BIN:\$PATH\""
    ;;
esac
