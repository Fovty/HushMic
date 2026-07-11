#!/usr/bin/env bash
# Build all hushmic release artifacts into dist/ (version from Cargo.toml):
#   * hushmic-<ver>-x86_64.tar.gz   (portable tarball + install.sh)
#   * hushmic_<ver>-1_amd64.deb     (Debian/Ubuntu package)
#   * hushmic-x86_64.AppImage       (self-contained AppImage)
#   * sha256sums.txt                (checksums over the above)
#
# Runnable locally and by CI. Requires: rust/cargo, cargo-deb, python3 (optional),
# curl/wget, fuse-less appimagetool (auto-downloaded + sha256-pinned). No FUSE.
#
# NOTE (glibc floor): artifacts inherit the BUILD HOST's glibc requirement —
# release builds must run on/in the oldest supported base (ubuntu:22.04,
# glibc 2.35); release.yml enforces this with an objdump check.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Single source of truth: the workspace package version (cargo-deb derives the
# .deb name from this too, so every artifact name stays consistent on a bump).
VERSION="$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
ARCH="x86_64"
NAME="hushmic-${VERSION}-${ARCH}"
DIST="$REPO_ROOT/dist"
TOOLS="$REPO_ROOT/.build-tools"

# Tray status-icon ladder (three SNI names x eight sizes) shipped alongside the
# app icon in every artifact; must stay in lockstep with packaging/tray/hicolor/
# and the names the tray requests over SNI.
TRAY_SIZES="16x16 22x22 24x24 32x32 48x48 64x64 128x128 256x256"
TRAY_NAMES="hushmic-tray hushmic-tray-off hushmic-tray-error"

# Install-layout paths to bake into the plugin for system builds.
export HUSHMIC_BUILD_MODEL="/usr/share/hushmic/models/dpdfnet8_48khz_hr.onnx"
export HUSHMIC_BUILD_DYLIB="/usr/lib/hushmic/libonnxruntime.so"

log() { printf '\n=== %s ===\n' "$*"; }

# ---------------------------------------------------------------------------
# 0. Assets + release build
# ---------------------------------------------------------------------------
log "Provisioning assets"
bash "$REPO_ROOT/scripts/setup-assets.sh"

log "Building release (baking install-layout paths)"
cargo build --release

BIN="$REPO_ROOT/target/release/hushmic"
PLUGIN="$REPO_ROOT/target/release/libdpdfnet_ladspa.so"
[ -x "$BIN" ] || { echo "error: $BIN not built" >&2; exit 1; }
[ -f "$PLUGIN" ] || { echo "error: $PLUGIN not built" >&2; exit 1; }

rm -rf "$DIST"
mkdir -p "$DIST"

# ---------------------------------------------------------------------------
# 1. Tarball
# ---------------------------------------------------------------------------
log "Assembling tarball"
STAGE="$DIST/.stage/$NAME"
rm -rf "$DIST/.stage"
# install -d -m 755 (not mkdir -p): directory modes land in the tarball too,
# and a hardened build-host umask would otherwise record 700/750 dirs that a
# root `tar -xzp` faithfully reproduces as unreadable-to-users directories.
install -d -m 755 "$STAGE/bin" "$STAGE/lib/ladspa" "$STAGE/lib/hushmic" \
         "$STAGE/share/hushmic/models" "$STAGE/share/applications" \
         "$STAGE/share/icons/hicolor/256x256/apps"
# install -m / explicit chmod everywhere: plain cp preserves source modes, and
# the models arrive 0600 from dpdfnet's download cache — shipped that way, a
# root extraction (tar -p is the default for root) leaves them unreadable to
# the user-session PipeWire that must load them.
install -m 755 "$BIN" "$STAGE/bin/hushmic"
install -m 644 "$PLUGIN" "$STAGE/lib/ladspa/libdpdfnet_ladspa.so"
cp -P "$REPO_ROOT"/assets/lib/libonnxruntime.so* "$STAGE/lib/hushmic/"
chmod 755 "$STAGE/lib/hushmic/"libonnxruntime.so*
install -m 644 "$REPO_ROOT"/assets/models/*.onnx "$STAGE/share/hushmic/models/"
install -m 644 "$REPO_ROOT/packaging/hushmic.desktop" "$STAGE/share/applications/hushmic.desktop"
install -m 644 "$REPO_ROOT/packaging/hushmic-256.png" "$STAGE/share/icons/hicolor/256x256/apps/hushmic.png"
# Tray status icons: explicit per-name installs (not a glob) so a missing size
# or state fails the build instead of silently shipping an incomplete ladder.
for size in $TRAY_SIZES; do
  install -d -m 755 "$STAGE/share/icons/hicolor/$size/status"
  for icon in $TRAY_NAMES; do
    install -m 644 "$REPO_ROOT/packaging/tray/hicolor/$size/status/$icon.png" \
             "$STAGE/share/icons/hicolor/$size/status/$icon.png"
  done
done
install -m 644 "$REPO_ROOT/LICENSE-MIT" "$STAGE/LICENSE-MIT"
install -m 644 "$REPO_ROOT/LICENSE-APACHE" "$STAGE/LICENSE-APACHE"
install -m 755 "$REPO_ROOT/scripts/install.sh" "$STAGE/install.sh"
# root-owned entries: extracting as root must not create files owned by uid 1000
tar -C "$DIST/.stage" --owner=0 --group=0 --numeric-owner \
    -czf "$DIST/${NAME}.tar.gz" "$NAME"
rm -rf "$DIST/.stage"
echo "  -> dist/${NAME}.tar.gz"

# ---------------------------------------------------------------------------
# 2. Debian package (reuse the env-baked release; do not rebuild)
# ---------------------------------------------------------------------------
log "Building .deb"
if ! command -v cargo-deb >/dev/null 2>&1; then
  echo "cargo-deb not found; installing..."
  cargo install cargo-deb
fi
cargo deb -p hushmic --no-build
DEB="$REPO_ROOT/target/debian/hushmic_${VERSION}-1_amd64.deb"
[ -f "$DEB" ] || { echo "error: expected $DEB" >&2; exit 1; }
cp "$DEB" "$DIST/"
echo "  -> dist/$(basename "$DEB")"

# ---------------------------------------------------------------------------
# 3. AppImage
# ---------------------------------------------------------------------------
log "Building AppImage"
APPDIR="$DIST/.AppDir"
rm -rf "$APPDIR"
install -d -m 755 "$APPDIR/usr/bin" "$APPDIR/usr/lib/ladspa" "$APPDIR/usr/lib" \
         "$APPDIR/usr/share/hushmic/models"
install -m 755 "$BIN" "$APPDIR/usr/bin/hushmic"
install -m 644 "$PLUGIN" "$APPDIR/usr/lib/ladspa/libdpdfnet_ladspa.so"
cp -P "$REPO_ROOT"/assets/lib/libonnxruntime.so* "$APPDIR/usr/lib/"
chmod 755 "$APPDIR/usr/lib/"libonnxruntime.so*
install -m 644 "$REPO_ROOT"/assets/models/*.onnx "$APPDIR/usr/share/hushmic/models/"
# Tray status icons: AppRun points HUSHMIC_TRAY_THEME_DIR at this tree so SNI
# hosts can resolve the names outside the system hicolor theme.
for size in $TRAY_SIZES; do
  install -d -m 755 "$APPDIR/usr/share/icons/hicolor/$size/status"
  for icon in $TRAY_NAMES; do
    install -m 644 "$REPO_ROOT/packaging/tray/hicolor/$size/status/$icon.png" \
             "$APPDIR/usr/share/icons/hicolor/$size/status/$icon.png"
  done
done
install -m755 "$REPO_ROOT/packaging/AppRun" "$APPDIR/AppRun"
install -m 644 "$REPO_ROOT/packaging/hushmic.desktop" "$APPDIR/hushmic.desktop"

# Icon: use the repo icon if present, else decode the embedded placeholder.
if [ -f "$REPO_ROOT/packaging/hushmic.png" ]; then
  cp "$REPO_ROOT/packaging/hushmic.png" "$APPDIR/hushmic.png"
else
  echo "  generating placeholder icon"
  base64 -d > "$APPDIR/hushmic.png" <<'ICON_B64'
iVBORw0KGgoAAAANSUhEUgAAAQAAAAEACAYAAABccqhmAAAFEUlEQVR4nO3dwXETWxBAUfsXQbDwmkAcg6MhAEdDDE7Nf8UCV1GWsDTz+t1z1i7Uoug7b2RJPH5/+vH+ACT9d/YAwHkEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMK+nT0Ax3r+9fPTn3l7eT1gElbw+P3px/vZQ3A/lyz8ZwRhXwKwqVss/kdCsB8B2Mw9Fv8jIdiHFwE3csTyH/k43J8TwAbOXEingdmcAIY7+2p89uPzNQIw2CrLt8ocXE8Ahlpt6Vabh8sIwECrLtuqc/F3AgBhAjDM6lfZ1efjTwIwyJTlmjInAgBpAjDEtKvqtHmrBADCBGCAqVfTqXOXCACECQCECQCECcDipt9HT59/dwIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQKwuLeX17NH+JLp8+9OACBMACBMACBMAAaYeh89de6Sb2cPsLvnXz8v+rnisvi7OZ8TwBDTlmDavFUCAGECMMiUq+qUORGAcVZfrtXn408CAGECsIhLXxF/eFj3KnvNXNc8X+5HAO7sXsu6WgQqz3M3AjDYKsuxyhxcTwCGO3v5zn58vkYAFvKv98VvL6+HL+JXHtP9/zoE4ABHLafH4Vo+C7CZ30tzj6ushdyPE8BibrW4t7wtuOWf5fi/FieAg7y9vJ7yj//j4l4ywwpX+hVmKBCABT3/+rnl79Vd/dfjFgDCBOBA5bfK7vBW5x0JwMJ2icAuz2NHAnCwa69u05fn2vld/Y8lABAmACeonAJc/dcnACfZPQKWfwYBGGRKBKbMycPD4/enH+9nD1H2lU8Arman51LhBHCyXT5Sa/lnEoAFTI+A5Z/LZwGG+718ZyzTKgHi33kNYCG3WKgjQjBlTj4nAIu55VX1lku26lx8jQAs6F5H6xU+jGT51yIAC9vpHtvir8lvARa2y9Ls8jx2JACLm7480+ffnVuAQSbdElj8GZwABpmyVFPmxAlgrBVPAxZ/HgEYboUQWPy5BGAjR8bA0u9BADblvwbjEgIQ8S9BsPD7E4CYS0Ng+Rv8GhDCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCfCXYYlb4mu9783Vj63ACgDABgDABgDABgDABgDABgDABgDABgDABgDDvBIQwJwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAI+x8xEwaUjw6b6gAAAABJRU5ErkJggg==
ICON_B64
fi

# Resolve appimagetool: $APPIMAGETOOL env, PATH, or auto-download. Downloads
# are PINNED to a tagged release + sha256 (the old 'continuous' tag is mutable:
# a compromised asset there would silently backdoor the shipped AppImage), and
# a cached copy is re-verified on every run.
AIT_VERSION="1.9.1"
AIT_URL="https://github.com/AppImage/appimagetool/releases/download/${AIT_VERSION}/appimagetool-x86_64.AppImage"
AIT_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
APPIMAGETOOL="${APPIMAGETOOL:-}"
if [ -z "$APPIMAGETOOL" ]; then
  if command -v appimagetool >/dev/null 2>&1; then
    APPIMAGETOOL="$(command -v appimagetool)"
  else
    mkdir -p "$TOOLS"
    APPIMAGETOOL="$TOOLS/appimagetool-x86_64.AppImage"
    if ! echo "$AIT_SHA256  $APPIMAGETOOL" | sha256sum -c - >/dev/null 2>&1; then
      echo "  downloading appimagetool ${AIT_VERSION}"
      if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$AIT_URL" -o "$APPIMAGETOOL"
      else
        wget -qO "$APPIMAGETOOL" "$AIT_URL"
      fi
      if ! echo "$AIT_SHA256  $APPIMAGETOOL" | sha256sum -c - >/dev/null 2>&1; then
        echo "error: appimagetool download does not match its pinned sha256; aborting." >&2
        exit 1
      fi
      chmod +x "$APPIMAGETOOL"
    fi
  fi
fi

# The type2 runtime gets EMBEDDED into the shipped AppImage and executes on
# every user's machine; appimagetool's default fetches it from the mutable
# 'continuous' tag. Pin it like appimagetool itself: tagged release + sha256,
# cached copy re-verified on every run.
RT_VERSION="20251108"
RT_URL="https://github.com/AppImage/type2-runtime/releases/download/${RT_VERSION}/runtime-${ARCH}"
RT_SHA256="2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"  # x86_64
RUNTIME_FILE="$TOOLS/runtime-${ARCH}-${RT_VERSION}"
mkdir -p "$TOOLS"
if ! echo "$RT_SHA256  $RUNTIME_FILE" | sha256sum -c - >/dev/null 2>&1; then
  echo "  downloading type2 runtime ${RT_VERSION}"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$RT_URL" -o "$RUNTIME_FILE"
  else
    wget -qO "$RUNTIME_FILE" "$RT_URL"
  fi
  if ! echo "$RT_SHA256  $RUNTIME_FILE" | sha256sum -c - >/dev/null 2>&1; then
    echo "error: type2 runtime download does not match its pinned sha256; aborting." >&2
    exit 1
  fi
fi

# --appimage-extract-and-run avoids needing FUSE in CI/sandboxes.
# appimagetool reads the target arch from $ARCH.
export ARCH
"$APPIMAGETOOL" --appimage-extract-and-run \
  --runtime-file "$RUNTIME_FILE" \
  --no-appstream "$APPDIR" "$DIST/hushmic-${ARCH}.AppImage"
chmod +x "$DIST/hushmic-${ARCH}.AppImage"
rm -rf "$APPDIR"
echo "  -> dist/hushmic-${ARCH}.AppImage"

# ---------------------------------------------------------------------------
# 4. Checksums
# ---------------------------------------------------------------------------
log "Computing checksums"
( cd "$DIST" && sha256sum ./*.tar.gz ./*.deb ./*.AppImage > sha256sums.txt )
cat "$DIST/sha256sums.txt"

log "Done. Artifacts in dist/:"
ls -lh "$DIST"
