#!/usr/bin/env bash
# Build all hushmic release artifacts into dist/ (version from Cargo.toml):
#   * hushmic-<ver>-x86_64.tar.gz   (portable tarball + install.sh)
#   * hushmic_<ver>-1_amd64.deb     (Debian/Ubuntu package)
#   * hushmic_amd64.deb             (the same file, for links without a version)
#   * hushmic-x86_64.AppImage       (self-contained AppImage, + .zsync)
#   * <model>.onnx, <model>.weights.f32 (standalone models + native weights)
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

# The app icon at every hicolor size, plus scalable/apps/hushmic.svg; must
# stay in lockstep with packaging/tray/hicolor/ (packaging/tray/make-icons.py).
APP_ICON_SIZES="16x16 22x22 24x24 32x32 48x48 64x64 128x128 256x256 512x512"
# Tray status-icon ladder (five SNI names x eight sizes) shipped alongside the
# app icon in every artifact; must stay in lockstep with packaging/tray/hicolor/
# and the names the tray requests over SNI.
TRAY_SIZES="16x16 22x22 24x24 32x32 48x48 64x64 128x128 256x256"
TRAY_NAMES="hushmic-tray hushmic-tray-off hushmic-tray-bypass hushmic-tray-mute hushmic-tray-error"
# The monochrome set the desktop recolors (issue #17), same five states under
# the same stems plus a -symbolic suffix. Shipped in GNOME's and Plasma's
# panel sizes plus scalable/ for everything else. hicolor's fixed directories
# only match at their own scale, hence the @2 twins.
TRAY_SYMBOLIC_DIRS="16x16 16x16@2 22x22 22x22@2 24x24 24x24@2 scalable"

# Install-layout paths to bake into the plugin for system builds.
export HUSHMIC_BUILD_MODEL="/usr/share/hushmic/models/dpdfnet8_48khz_hr.onnx"
export HUSHMIC_BUILD_DYLIB="/usr/lib/hushmic/libonnxruntime.so"

log() { printf '\n=== %s ===\n' "$*"; }

# install_app_icons <hicolor dir>: the app icon at every size plus scalable/,
# one explicit install per file so a missing size fails the build. Every
# directory level gets 755 (see the umask note at the tarball).
install_app_icons() {
  install -d -m 755 "${1%/*}" "$1"
  for size in $APP_ICON_SIZES; do
    install -d -m 755 "$1/$size" "$1/$size/apps"
    install -m 644 "$REPO_ROOT/packaging/tray/hicolor/$size/apps/hushmic.png" "$1/$size/apps/hushmic.png"
  done
  install -d -m 755 "$1/scalable" "$1/scalable/apps"
  install -m 644 "$REPO_ROOT/packaging/tray/hicolor/scalable/apps/hushmic.svg" "$1/scalable/apps/hushmic.svg"
}

# ---------------------------------------------------------------------------
# 0. Assets + release build
# ---------------------------------------------------------------------------
log "Provisioning assets"
bash "$REPO_ROOT/scripts/setup-assets.sh"

log "Building release (baking install-layout paths)"
# Opus recordings decode through libopus, linked in statically: the
# tarball, the .deb, the AppImage and hushmic-bin then run without the
# system's libopus (opusic-sys links libopus.a from libopus-dev when
# OPUS_LIB_STATIC is set). Distro-built packages link the system one.
# rustc bundles a static library into the crate itself and looks for it
# only in -L folders, not the linker's default ones: OPUS_LIB_DIR names
# the folder.
OPUS_A="$(gcc -print-file-name=libopus.a 2>/dev/null || true)"
case "$OPUS_A" in
  /*) OPUS_A="$(readlink -f "$OPUS_A")" ;;
  *) echo "error: libopus.a not found on the build host (apt install libopus-dev)" >&2; exit 1 ;;
esac
# HUSHMIC_EXTRA_FEATURES: more cargo features, comma-separated, for a build
# to try them (hushmic/speakers); a release sets none.
OPUS_LIB_STATIC=true OPUS_LIB_DIR="$(dirname "$OPUS_A")" \
  cargo build --release --features "hushmic/opus${HUSHMIC_EXTRA_FEATURES:+,$HUSHMIC_EXTRA_FEATURES}"

BIN="$REPO_ROOT/target/release/hushmic"
PLUGIN="$REPO_ROOT/target/release/libdpdfnet_ladspa.so"
[ -x "$BIN" ] || { echo "error: $BIN not built" >&2; exit 1; }
[ -f "$PLUGIN" ] || { echo "error: $PLUGIN not built" >&2; exit 1; }
# A cached build from before the static link would still need libopus.so.
if objdump -p "$BIN" | grep -q 'NEEDED.*libopus'; then
  echo "error: $BIN links libopus.so; release builds link it statically" >&2
  exit 1
fi

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
         "$STAGE/share/hushmic/models" "$STAGE/share/applications"
# install -m / explicit chmod everywhere: plain cp preserves source modes, and
# the models arrive 0600 from dpdfnet's download cache — shipped that way, a
# root extraction (tar -p is the default for root) leaves them unreadable to
# the user-session PipeWire that must load them.
install -m 755 "$BIN" "$STAGE/bin/hushmic"
install -m 644 "$PLUGIN" "$STAGE/lib/ladspa/libdpdfnet_ladspa.so"
cp -P "$REPO_ROOT"/assets/lib/libonnxruntime.so* "$STAGE/lib/hushmic/"
chmod 755 "$STAGE/lib/hushmic/"libonnxruntime.so*
install -m 644 "$REPO_ROOT"/assets/models/*.onnx "$STAGE/share/hushmic/models/"
# Native engine weights (issue #18), next to the models they belong to.
# Explicit names so a missing file fails the build: without them every
# install silently runs ONNX.
install -m 644 "$REPO_ROOT/assets/models/dpdfnet8_48khz_hr.weights.f32" \
               "$REPO_ROOT/assets/models/dpdfnet2_48khz_hr.weights.f32" \
               "$STAGE/share/hushmic/models/"
install -m 644 "$REPO_ROOT/packaging/hushmic.desktop" "$STAGE/share/applications/hushmic.desktop"
install -m 644 "$REPO_ROOT/packaging/hushmic-clean.desktop" "$STAGE/share/applications/hushmic-clean.desktop"
install -d -m 755 "$STAGE/lib/systemd/user"
install -m 644 "$REPO_ROOT/packaging/systemd/hushmic.service" "$STAGE/lib/systemd/user/hushmic.service"
install_app_icons "$STAGE/share/icons/hicolor"
# Tray status icons: explicit per-name installs (not a glob) so a missing size
# or state fails the build instead of silently shipping an incomplete ladder.
for size in $TRAY_SIZES; do
  install -d -m 755 "$STAGE/share/icons/hicolor/$size/status"
  for icon in $TRAY_NAMES; do
    install -m 644 "$REPO_ROOT/packaging/tray/hicolor/$size/status/$icon.png" \
             "$STAGE/share/icons/hicolor/$size/status/$icon.png"
  done
done
for size in $TRAY_SYMBOLIC_DIRS; do
  install -d -m 755 "$STAGE/share/icons/hicolor/$size/status"
  for icon in $TRAY_NAMES; do
    install -m 644 "$REPO_ROOT/packaging/tray/hicolor/$size/status/hushmic-mono${icon#hushmic-tray}-symbolic.svg" \
             "$STAGE/share/icons/hicolor/$size/status/hushmic-mono${icon#hushmic-tray}-symbolic.svg"
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
# The same package under a name without the version: what the website and the
# README link as releases/latest/download/hushmic_amd64.deb, so those links hold
# from one release to the next. A release is immutable once published: this
# file has to be among its assets from the start (release.yml lists it).
cp "$DEB" "$DIST/hushmic_amd64.deb"
echo "  -> dist/hushmic_amd64.deb"

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
# winit dlopens libxkbcommon-x11 for its X11 windows; minimal X11 systems
# (and the AppImage catalog's test runner) lack it and the window panics on
# open. Bundle it with its libxcb-xkb dependency; NOT
# libxkbcommon itself, which is everywhere and must match the host's.
# (libopus is linked into the binary.)
for lib in libxkbcommon-x11.so.0 libxcb-xkb.so.1; do
  src=$(ldconfig -p | awk -v l="$lib" '$1 == l && /x86-64/ { print $NF; exit }')
  [ -n "$src" ] || { echo "error: $lib not found on the build host (apt install libxkbcommon-x11-0 libxcb-xkb1)" >&2; exit 1; }
  install -m 644 "$(readlink -f "$src")" "$APPDIR/usr/lib/$lib"
done
install -m 644 "$REPO_ROOT"/assets/models/*.onnx "$APPDIR/usr/share/hushmic/models/"
install -m 644 "$REPO_ROOT/assets/models/dpdfnet8_48khz_hr.weights.f32" \
               "$REPO_ROOT/assets/models/dpdfnet2_48khz_hr.weights.f32" \
               "$APPDIR/usr/share/hushmic/models/"
# Tray status icons: AppRun points HUSHMIC_TRAY_THEME_DIR at this tree so SNI
# hosts can resolve the names outside the system hicolor theme.
for size in $TRAY_SIZES; do
  install -d -m 755 "$APPDIR/usr/share/icons/hicolor/$size/status"
  for icon in $TRAY_NAMES; do
    install -m 644 "$REPO_ROOT/packaging/tray/hicolor/$size/status/$icon.png" \
             "$APPDIR/usr/share/icons/hicolor/$size/status/$icon.png"
  done
done
for size in $TRAY_SYMBOLIC_DIRS; do
  install -d -m 755 "$APPDIR/usr/share/icons/hicolor/$size/status"
  for icon in $TRAY_NAMES; do
    install -m 644 "$REPO_ROOT/packaging/tray/hicolor/$size/status/hushmic-mono${icon#hushmic-tray}-symbolic.svg" \
             "$APPDIR/usr/share/icons/hicolor/$size/status/hushmic-mono${icon#hushmic-tray}-symbolic.svg"
  done
done
# The app icon ladder too, for AppImage integration tools that install it.
install_app_icons "$APPDIR/usr/share/icons/hicolor"
install -m755 "$REPO_ROOT/packaging/AppRun" "$APPDIR/AppRun"
install -m 644 "$REPO_ROOT/packaging/hushmic.desktop" "$APPDIR/hushmic.desktop"
# AppStream metainfo (the AppImage catalog reads summary, license and
# screenshot from it): the Flatpak copy, launchable pointed at our desktop id.
# The catalog only picks up the *.appdata.xml spelling.
install -d -m 755 "$APPDIR/usr/share/metainfo"
# vcs-browser is dropped: the catalog's older appstreamcli rejects it.
sed -e 's|<launchable type="desktop-id">io.github.fovty.HushMic.desktop</launchable>|<launchable type="desktop-id">hushmic.desktop</launchable>|' \
    -e '/<url type="vcs-browser">/d' \
  "$REPO_ROOT/packaging/flatpak/io.github.fovty.HushMic.metainfo.xml" \
  > "$APPDIR/usr/share/metainfo/io.github.fovty.HushMic.appdata.xml"
install -d -m 755 "$APPDIR/usr/share/applications"
install -m 644 "$REPO_ROOT/packaging/hushmic.desktop" "$APPDIR/usr/share/applications/hushmic.desktop"
grep -q '<launchable type="desktop-id">hushmic.desktop</launchable>' \
  "$APPDIR/usr/share/metainfo/io.github.fovty.HushMic.appdata.xml" \
  || { echo "error: AppImage metainfo launchable was not rewritten" >&2; exit 1; }

# The AppImage's own icon (appimagetool also makes .DirIcon from it).
install -m 644 "$REPO_ROOT/packaging/hushmic-256.png" "$APPDIR/hushmic.png"

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
# Update information lets AppImageUpdate (and the catalog) fetch only the
# changed blocks of the latest release. appimagetool writes the matching
# .zsync into its WORKING directory (not next to the output), hence the cd;
# it is published alongside the AppImage.
( cd "$DIST" && "$APPIMAGETOOL" --appimage-extract-and-run \
  --runtime-file "$RUNTIME_FILE" \
  -u "gh-releases-zsync|Fovty|HushMic|latest|hushmic-${ARCH}.AppImage.zsync" \
  --no-appstream "$APPDIR" "$DIST/hushmic-${ARCH}.AppImage" )
chmod +x "$DIST/hushmic-${ARCH}.AppImage"
[ -s "$DIST/hushmic-${ARCH}.AppImage.zsync" ] \
  || { echo "error: appimagetool wrote no .zsync file (an appimagetool without its bundled zsyncmake?)" >&2; exit 1; }
rm -rf "$APPDIR"
echo "  -> dist/hushmic-${ARCH}.AppImage (+ .zsync)"

# ---------------------------------------------------------------------------
# 4. Standalone model files (for hushmic-denoiser library consumers)
# ---------------------------------------------------------------------------
# The DPDFNet models (Apache-2.0, Ceva) already ship inside the .deb/AppImage;
# publishing them as raw release assets is what lets Rust apps embedding the
# hushmic-denoiser crate download a model without installing HushMic.
log "Copying model assets"
cp "$REPO_ROOT/assets/models/dpdfnet8_48khz_hr.onnx" \
   "$REPO_ROOT/assets/models/dpdfnet2_48khz_hr.onnx" "$DIST/"
# The native weights too: a mirror of the pinned upstream files that the
# source builds (setup-assets.sh, Flatpak, Nix) fall back to.
install -m 644 "$REPO_ROOT/assets/models/dpdfnet8_48khz_hr.weights.f32" \
               "$REPO_ROOT/assets/models/dpdfnet2_48khz_hr.weights.f32" "$DIST/"

# ---------------------------------------------------------------------------
# 5. Checksums
# ---------------------------------------------------------------------------
log "Computing checksums"
( cd "$DIST" && sha256sum ./*.tar.gz ./*.deb ./*.AppImage ./*.AppImage.zsync ./*.onnx ./*.weights.f32 > sha256sums.txt )
cat "$DIST/sha256sums.txt"

log "Done. Artifacts in dist/:"
ls -lh "$DIST"
