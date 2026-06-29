#!/usr/bin/env bash
# Build all hushmic v0.1.0 release artifacts into dist/:
#   * hushmic-0.1.0-x86_64.tar.gz   (portable tarball + install.sh)
#   * hushmic_0.1.0-1_amd64.deb     (Debian/Ubuntu package)
#   * hushmic-x86_64.AppImage       (self-contained AppImage)
#   * sha256sums.txt                (checksums over the above)
#
# Runnable locally and by CI. Requires: rust/cargo, cargo-deb, python3 (optional),
# curl/wget, fuse-less appimagetool (auto-downloaded). No FUSE needed.
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
mkdir -p "$STAGE/bin" "$STAGE/lib/ladspa" "$STAGE/lib/hushmic" \
         "$STAGE/share/hushmic/models" "$STAGE/share/applications" \
         "$STAGE/share/icons/hicolor/256x256/apps"
cp "$BIN" "$STAGE/bin/hushmic"
cp "$PLUGIN" "$STAGE/lib/ladspa/libdpdfnet_ladspa.so"
cp -P "$REPO_ROOT"/assets/lib/libonnxruntime.so* "$STAGE/lib/hushmic/"
cp "$REPO_ROOT"/assets/models/*.onnx "$STAGE/share/hushmic/models/"
cp "$REPO_ROOT/packaging/hushmic.desktop" "$STAGE/share/applications/hushmic.desktop"
cp "$REPO_ROOT/packaging/hushmic-256.png" "$STAGE/share/icons/hicolor/256x256/apps/hushmic.png"
cp "$REPO_ROOT/LICENSE-MIT" "$STAGE/LICENSE-MIT"
cp "$REPO_ROOT/LICENSE-APACHE" "$STAGE/LICENSE-APACHE"
cp "$REPO_ROOT/scripts/install.sh" "$STAGE/install.sh"
chmod +x "$STAGE/install.sh" "$STAGE/bin/hushmic"
tar -C "$DIST/.stage" -czf "$DIST/${NAME}.tar.gz" "$NAME"
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
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib/ladspa" "$APPDIR/usr/lib" \
         "$APPDIR/usr/share/hushmic/models"
cp "$BIN" "$APPDIR/usr/bin/hushmic"
cp "$PLUGIN" "$APPDIR/usr/lib/ladspa/libdpdfnet_ladspa.so"
cp -P "$REPO_ROOT"/assets/lib/libonnxruntime.so* "$APPDIR/usr/lib/"
cp "$REPO_ROOT"/assets/models/*.onnx "$APPDIR/usr/share/hushmic/models/"
install -m755 "$REPO_ROOT/packaging/AppRun" "$APPDIR/AppRun"
cp "$REPO_ROOT/packaging/hushmic.desktop" "$APPDIR/hushmic.desktop"

# Icon: use the repo icon if present, else decode the embedded placeholder.
if [ -f "$REPO_ROOT/packaging/hushmic.png" ]; then
  cp "$REPO_ROOT/packaging/hushmic.png" "$APPDIR/hushmic.png"
else
  echo "  generating placeholder icon"
  base64 -d > "$APPDIR/hushmic.png" <<'ICON_B64'
iVBORw0KGgoAAAANSUhEUgAAAQAAAAEACAYAAABccqhmAAAFEUlEQVR4nO3dwXETWxBAUfsXQbDwmkAcg6MhAEdDDE7Nf8UCV1GWsDTz+t1z1i7Uoug7b2RJPH5/+vH+ACT9d/YAwHkEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMIEAMK+nT0Ax3r+9fPTn3l7eT1gElbw+P3px/vZQ3A/lyz8ZwRhXwKwqVss/kdCsB8B2Mw9Fv8jIdiHFwE3csTyH/k43J8TwAbOXEingdmcAIY7+2p89uPzNQIw2CrLt8ocXE8Ahlpt6Vabh8sIwECrLtuqc/F3AgBhAjDM6lfZ1efjTwIwyJTlmjInAgBpAjDEtKvqtHmrBADCBGCAqVfTqXOXCACECQCECQCECcDipt9HT59/dwIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQIAYQKwuLeX17NH+JLp8+9OACBMACBMACBMAAaYeh89de6Sb2cPsLvnXz8v+rnisvi7OZ8TwBDTlmDavFUCAGECMMiUq+qUORGAcVZfrtXn408CAGECsIhLXxF/eFj3KnvNXNc8X+5HAO7sXsu6WgQqz3M3AjDYKsuxyhxcTwCGO3v5zn58vkYAFvKv98VvL6+HL+JXHtP9/zoE4ABHLafH4Vo+C7CZ30tzj6ushdyPE8BibrW4t7wtuOWf5fi/FieAg7y9vJ7yj//j4l4ywwpX+hVmKBCABT3/+rnl79Vd/dfjFgDCBOBA5bfK7vBW5x0JwMJ2icAuz2NHAnCwa69u05fn2vld/Y8lABAmACeonAJc/dcnACfZPQKWfwYBGGRKBKbMycPD4/enH+9nD1H2lU8Arman51LhBHCyXT5Sa/lnEoAFTI+A5Z/LZwGG+718ZyzTKgHi33kNYCG3WKgjQjBlTj4nAIu55VX1lku26lx8jQAs6F5H6xU+jGT51yIAC9vpHtvir8lvARa2y9Ls8jx2JACLm7480+ffnVuAQSbdElj8GZwABpmyVFPmxAlgrBVPAxZ/HgEYboUQWPy5BGAjR8bA0u9BADblvwbjEgIQ8S9BsPD7E4CYS0Ng+Rv8GhDCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCBADCfCXYYlb4mu9783Vj63ACgDABgDABgDABgDABgDABgDABgDABgDABgDDvBIQwJwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAIEwAI+x8xEwaUjw6b6gAAAABJRU5ErkJggg==
ICON_B64
fi

# Resolve appimagetool: $APPIMAGETOOL env, PATH, or auto-download (FUSE-less).
APPIMAGETOOL="${APPIMAGETOOL:-}"
if [ -z "$APPIMAGETOOL" ]; then
  if command -v appimagetool >/dev/null 2>&1; then
    APPIMAGETOOL="$(command -v appimagetool)"
  else
    mkdir -p "$TOOLS"
    APPIMAGETOOL="$TOOLS/appimagetool-x86_64.AppImage"
    if [ ! -x "$APPIMAGETOOL" ]; then
      echo "  downloading appimagetool"
      ait_url="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
      if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$ait_url" -o "$APPIMAGETOOL"
      else
        wget -qO "$APPIMAGETOOL" "$ait_url"
      fi
      chmod +x "$APPIMAGETOOL"
    fi
  fi
fi

# --appimage-extract-and-run avoids needing FUSE in CI/sandboxes.
# appimagetool reads the target arch from $ARCH.
export ARCH
"$APPIMAGETOOL" --appimage-extract-and-run \
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
