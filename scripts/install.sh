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

# Pin a known umask: a hardened caller umask (077/027 — common on CIS/STIG
# boxes, and preserved across sudo on some distros) would otherwise make the
# root-created dirs and copied libs unreadable to the user-session PipeWire
# that has to load the plugin and models (install "succeeds", mic is silent).
# Same fix as the Nix installer; the explicit modes below are a second layer.
umask 022

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
DEST_HICOLOR="$PREFIX/share/icons/hicolor"
DEST_LICENSES="$PREFIX/share/licenses/hushmic"

# Tray status-icon ladder shipped in the payload (five SNI names x eight
# sizes, status/ context); must stay in lockstep with packaging/tray/hicolor/.
TRAY_SIZES="16x16 22x22 24x24 32x32 48x48 64x64 128x128 256x256"
TRAY_NAMES="hushmic-tray hushmic-tray-off hushmic-tray-bypass hushmic-tray-mute hushmic-tray-error"

refresh_icon_cache() {
  # Refresh the hicolor cache so SNI hosts pick icon changes up without a
  # re-login; a missing tool or a cache-less prefix (no index.theme — normal
  # for a fresh ~/.local) is fine, hence the '|| true'.
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    as_root gtk-update-icon-cache -f "$DEST_HICOLOR" >/dev/null 2>&1 || true
  fi
}

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
# The tray's "Start on login" toggle writes a per-user autostart entry; left
# behind, every login would relaunch a binary that no longer exists. Under
# sudo, resolve the INVOKING user's home (root's $HOME is not theirs).
remove_autostart_entry() {
  if [ -n "${SUDO_USER:-}" ]; then
    _as_home="$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6)"
    _as_conf="${_as_home:+$_as_home/.config}"
  else
    _as_conf="${XDG_CONFIG_HOME:-${HOME:-}/.config}"
  fi
  [ -n "$_as_conf" ] && rm -f "$_as_conf/autostart/hushmic.desktop" 2>/dev/null
  return 0
}

do_uninstall() {
  echo "Removing hushmic from prefix: $PREFIX"
  as_root rm -f "$DEST_BIN/hushmic" "$DEST_BIN/hushmic-uninstall"
  as_root rm -f "$DEST_LADSPA/libdpdfnet_ladspa.so"
  as_root rm -rf "$DEST_LIB"
  as_root rm -rf "$PREFIX/share/hushmic"
  as_root rm -f "$DEST_APPS/hushmic.desktop"
  as_root rm -f "$DEST_ICONS/hushmic.png"
  for _s in $TRAY_SIZES; do
    for _n in $TRAY_NAMES; do
      as_root rm -f "$DEST_HICOLOR/$_s/status/$_n.png"
    done
  done
  refresh_icon_cache
  as_root rm -rf "$DEST_LICENSES"
  remove_autostart_entry
  echo "Done. (Per-user config under ~/.config/hushmic was left untouched.)"
}

if [ "$ACTION" = "uninstall" ]; then
  do_uninstall
  exit 0
fi

# ---------------------------------------------------------------------------
# Read-only prefix preflight
# ---------------------------------------------------------------------------
# On immutable distros (ostree-based Bazzite/Silverblue/Kinoite, A/B-image
# SteamOS) /usr is read-only even for root: without this check the install
# dies mid-payload with a raw EROFS from cp. Detect it before downloading
# anything and point at the options that actually work there.
_probe_dir="$PREFIX"
while [ ! -d "$_probe_dir" ]; do _probe_dir=$(dirname "$_probe_dir"); done
_ro_prefix=""
if command -v findmnt >/dev/null 2>&1; then
  case ",$(findmnt -no OPTIONS --target "$_probe_dir" 2>/dev/null)," in
    *,ro,*) _ro_prefix="yes" ;;
  esac
else
  # No findmnt: probe with a scratch dir at the privilege the install would
  # use, and diagnose read-only ONLY on an actual EROFS message — a sudo
  # auth failure or EACCES must not masquerade as an immutable distro (it
  # will surface with its own error at the first real install step instead).
  _probe="$_probe_dir/.hushmic-rw-probe.$$"
  trap '[ -n "${_probe:-}" ] && as_root rmdir "$_probe" 2>/dev/null || true' INT TERM EXIT
  if _probe_err="$(LC_ALL=C as_root mkdir "$_probe" 2>&1)"; then
    as_root rmdir "$_probe" 2>/dev/null || true
  else
    case "$_probe_err" in
      *"Read-only file system"*) _ro_prefix="yes" ;;
    esac
  fi
  trap - INT TERM EXIT
  _probe=""
fi
if [ -n "$_ro_prefix" ]; then
  cat >&2 <<EOF
error: '$PREFIX' is on a read-only filesystem.

This looks like an immutable distro (Bazzite, Silverblue, Kinoite,
SteamOS, ...), where even root cannot write to /usr. Options that work:

  AppImage — nothing to install at all:
      https://github.com/Fovty/hushmic/releases/latest

  User-local install (no sudo needed):
      curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sh -s -- --prefix "\$HOME/.local"

  /usr/local — writable on ostree-based systems (Bazzite, Silverblue,
  Kinoite — not SteamOS, whose /usr/local is read-only too):
      curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sudo sh -s -- --prefix /usr/local
EOF
  exit 1
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

# Co-located mode needs $0 to be a REAL script file sitting next to a release
# payload: under `curl | sh` $0 is just "sh" and SCRIPT_DIR resolves to the
# CALLER'S CWD, which must never be mistaken for a payload (e.g. running the
# one-liner from inside an old extracted release would silently install that
# stale version instead of downloading the latest).
if [ -f "$0" ] && [ -f "$SCRIPT_DIR/bin/hushmic" ] \
   && [ -f "$SCRIPT_DIR/lib/ladspa/libdpdfnet_ladspa.so" ]; then
  PAYLOAD="$SCRIPT_DIR"
  echo "Installing from co-located release payload: $PAYLOAD"
else
  echo "No co-located payload; downloading the latest release..."
  if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    echo "error: need curl or wget to download the release." >&2
    exit 1
  fi
  fetch() { if command -v curl >/dev/null 2>&1; then curl -fsSL "$1"; else wget -qO- "$1"; fi; }
  # The checksums file has a stable name under releases/latest/, so it both
  # locates the current tarball (no GitHub API — no rate limits, no JSON
  # scraping) and lets us VERIFY the download before installing it as root.
  SUMS_URL="https://github.com/${REPO}/releases/latest/download/sha256sums.txt"
  if ! SUMS="$(fetch "$SUMS_URL")"; then
    echo "error: could not download $SUMS_URL" >&2
    echo "       (network problem, or no published release?)" >&2
    exit 1
  fi
  ENTRY="$(printf '%s\n' "$SUMS" | grep -E "hushmic-[0-9][^ ]*-${ARCH}\.tar\.gz\$" | head -1)"
  if [ -z "$ENTRY" ]; then
    echo "error: no hushmic-*-${ARCH}.tar.gz entry in the release's sha256sums.txt." >&2
    exit 1
  fi
  TGZ_SHA="${ENTRY%% *}"
  TGZ_NAME="$(basename "${ENTRY##* }")"
  CLEANUP_DIR="$(mktemp -d)"
  tgz="$CLEANUP_DIR/$TGZ_NAME"
  # Guarded explicitly: wget -q would otherwise die with NO diagnostic under
  # set -e (e.g. a 404 when a new release is published between the sums fetch
  # and this one, flipping what 'latest' points at).
  dl_url="https://github.com/${REPO}/releases/latest/download/${TGZ_NAME}"
  dl_ok=1
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$dl_url" -o "$tgz" || dl_ok=0
  else
    wget -qO "$tgz" "$dl_url" || dl_ok=0
  fi
  if [ "$dl_ok" != 1 ]; then
    echo "error: could not download $TGZ_NAME from $dl_url" >&2
    echo "       (If a release was published seconds ago, 'latest' may have just changed — retry.)" >&2
    exit 1
  fi
  if ! printf '%s  %s\n' "$TGZ_SHA" "$tgz" | sha256sum -c - >/dev/null 2>&1; then
    echo "error: checksum mismatch for $TGZ_NAME — corrupted or tampered download; aborting." >&2
    echo "       (If a release was published seconds ago, just retry.)" >&2
    exit 1
  fi
  echo "Checksum verified."
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
ensure_dir() {
  # 0755 regardless of umask; 'install -d' (unlike 'mkdir -p -m') also gives
  # any parents it creates a umask-independent 0755. Dirs that already exist
  # are left untouched — we don't rewrite modes on system dirs we don't own.
  [ -d "$1" ] || as_root install -d -m 0755 "$1"
}

install_file() {
  # install_file <src> <dest-dir> <mode>
  src="$1"; destdir="$2"; mode="$3"
  ensure_dir "$destdir"
  as_root cp -f "$src" "$destdir/"
  as_root chmod "$mode" "$destdir/$(basename "$src")"
}

echo "Installing to prefix: $PREFIX"
install_file "$PAYLOAD/bin/hushmic" "$DEST_BIN" 755
install_file "$PAYLOAD/lib/ladspa/libdpdfnet_ladspa.so" "$DEST_LADSPA" 644

# ONNX Runtime shared lib(s) — preserve the symlink chain. cp gives a new
# file (source mode & ~umask), so pin the mode explicitly; chmod dereferences
# the symlinks, which pins the real file behind them too.
ensure_dir "$DEST_LIB"
as_root cp -Pf "$PAYLOAD"/lib/hushmic/libonnxruntime.so* "$DEST_LIB/"
as_root chmod 0755 "$DEST_LIB"/libonnxruntime.so*

# Models.
ensure_dir "$DEST_MODELS"
for m in "$PAYLOAD"/share/hushmic/models/*.onnx; do
  install_file "$m" "$DEST_MODELS" 644
done

# Heal dirs left 0700 by the pre-v0.1.3 umask-sensitive installer: ensure_dir
# deliberately never rewrites EXISTING dirs, so upgraders would keep the broken
# modes forever. These dirs hold only hushmic's own payload (the ladspa dir's
# canonical mode is 0755 everywhere), so re-asserting 0755 is safe — it can
# only loosen, never break, and it un-breaks upgraded installs.
for d in "$DEST_LADSPA" "$DEST_LIB" "$PREFIX/share/hushmic" "$DEST_MODELS" "$DEST_LICENSES"; do
  if [ -d "$d" ]; then
    as_root chmod 0755 "$d"
  fi
done

install_file "$PAYLOAD/share/applications/hushmic.desktop" "$DEST_APPS" 644
[ -f "$PAYLOAD/share/icons/hicolor/256x256/apps/hushmic.png" ] && install_file "$PAYLOAD/share/icons/hicolor/256x256/apps/hushmic.png" "$DEST_ICONS" 644

# Tray status icons (guarded per file: pre-tray payloads simply lack them).
for _s in $TRAY_SIZES; do
  for _n in $TRAY_NAMES; do
    _icon="$PAYLOAD/share/icons/hicolor/$_s/status/$_n.png"
    if [ -f "$_icon" ]; then
      install_file "$_icon" "$DEST_HICOLOR/$_s/status" 644
    fi
  done
done
refresh_icon_cache
[ -f "$PAYLOAD/LICENSE-MIT" ] && install_file "$PAYLOAD/LICENSE-MIT" "$DEST_LICENSES" 644
[ -f "$PAYLOAD/LICENSE-APACHE" ] && install_file "$PAYLOAD/LICENSE-APACHE" "$DEST_LICENSES" 644

# ---------------------------------------------------------------------------
# Generate the uninstaller (bakes this prefix).
# ---------------------------------------------------------------------------
GEN_DIR="$(mktemp -d)"
uninstaller="$GEN_DIR/hushmic-uninstall"
# PREFIX is baked in single-quoted with embedded quotes escaped, so a prefix
# containing spaces, `"`, `$` or backticks neither breaks the generated script
# nor gets expanded by it; everything else is a fully-quoted heredoc.
prefix_sq="$(printf %s "$PREFIX" | sed "s/'/'\\\\''/g")"
{
  printf '%s\n' '#!/bin/sh' \
    '# Auto-generated by hushmic install.sh — removes the install at the prefix below.' \
    'set -eu' \
    "PREFIX='${prefix_sq}'" \
    "TRAY_SIZES='${TRAY_SIZES}'" \
    "TRAY_NAMES='${TRAY_NAMES}'"
  cat <<'UNINSTALL_EOF'
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  case "$PREFIX/" in
    "$HOME"/*) [ -w "$PREFIX" ] || { command -v sudo >/dev/null 2>&1 && SUDO="sudo"; } ;;
    *)          command -v sudo >/dev/null 2>&1 && SUDO="sudo" ;;
  esac
fi
$SUDO rm -f "$PREFIX/bin/hushmic" "$PREFIX/bin/hushmic-uninstall"
$SUDO rm -f "$PREFIX/lib/ladspa/libdpdfnet_ladspa.so"
$SUDO rm -rf "$PREFIX/lib/hushmic"
$SUDO rm -rf "$PREFIX/share/hushmic"
$SUDO rm -f "$PREFIX/share/applications/hushmic.desktop"
$SUDO rm -f "$PREFIX/share/icons/hicolor/256x256/apps/hushmic.png"
for _s in $TRAY_SIZES; do
  for _n in $TRAY_NAMES; do
    $SUDO rm -f "$PREFIX/share/icons/hicolor/$_s/status/$_n.png"
  done
done
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  $SUDO gtk-update-icon-cache -f "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi
$SUDO rm -rf "$PREFIX/share/licenses/hushmic"
# Also drop the per-user autostart entry (would relaunch a removed binary).
if [ -n "${SUDO_USER:-}" ]; then
  _h="$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6)"
  _c="${_h:+$_h/.config}"
else
  _c="${XDG_CONFIG_HOME:-${HOME:-}/.config}"
fi
[ -n "$_c" ] && rm -f "$_c/autostart/hushmic.desktop" 2>/dev/null || true
echo "hushmic uninstalled from $PREFIX (config in ~/.config/hushmic left intact)."
UNINSTALL_EOF
} > "$uninstaller"
chmod +x "$uninstaller"
install_file "$uninstaller" "$DEST_BIN" 755

# ---------------------------------------------------------------------------
# Next steps
# ---------------------------------------------------------------------------
echo
echo "hushmic ${VERSION} installed."
echo
echo "Start it:                hushmic          (tray + A/B window; --tray = tray only)"
if [ -f "$0" ]; then
  echo "Uninstall:               hushmic-uninstall   (or: $0 --uninstall --prefix \"$PREFIX\")"
else
  # piped via `curl | sh`: $0 is just "sh", useless as a re-run hint
  echo "Uninstall:               hushmic-uninstall"
fi
# The binary's compiled-in default paths are exactly /usr — every OTHER prefix
# (including /usr/local) needs the env exports below or the virtual mic will
# silently fail to find the plugin/models.
case "$PREFIX" in
  /usr) : ;;
  *)
    echo
    echo "NOTE: you installed under a non-standard prefix. The binary's compiled-in"
    echo "default paths point at /usr; export these so it finds the bundled assets:"
    echo "  export ORT_DYLIB_PATH=\"$DEST_LIB/libonnxruntime.so\""
    echo "  export HUSHMIC_MODEL_DIR=\"$DEST_MODELS\""
    echo "  export HUSHMIC_PLUGIN_SO=\"$DEST_LADSPA/libdpdfnet_ladspa.so\""
    echo "  export HUSHMIC_TRAY_THEME_DIR=\"$PREFIX/share/icons\""
    echo "  export PATH=\"$DEST_BIN:\$PATH\""
    ;;
esac
