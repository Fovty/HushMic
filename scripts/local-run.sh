#!/usr/bin/env bash
# Build this checkout, install it to ~/.local/opt/hushmic (leaves the AUR /usr install alone),
# restart HushMic from there, and fail unless the live engine is native w7a8 avx-vnni.
# Usage: scripts/local-run.sh [--no-build]
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
P=${HUSHMIC_LOCAL_PREFIX:-$HOME/.local/opt/hushmic}
LOG=${XDG_STATE_HOME:-$HOME/.local/state}/hushmic-local.log
WANT="native w7a8 avx-vnni"

grep -qw avx_vnni /proc/cpuinfo || { echo "this CPU has no AVX-VNNI" >&2; exit 1; }

if [[ ${1:-} != --no-build ]]; then
  [[ -f $REPO/assets/models/dpdfnet8_48khz_hr.weights.f32 ]] || "$REPO/scripts/setup-assets.sh"
  (cd "$REPO" && HUSHMIC_BUILD_MODEL=$P/share/hushmic/models/dpdfnet8_48khz_hr.onnx \
    HUSHMIC_BUILD_DYLIB=$P/lib/hushmic/libonnxruntime.so \
    mise exec rust@stable -- cargo build --release --locked -p hushmic -p dpdfnet-ladspa)
fi

# The layout Paths::resolve looks for next to <prefix>/bin/hushmic: no env vars needed.
install -Dm755 "$REPO/target/release/hushmic" "$P/bin/hushmic"
install -Dm755 "$REPO/target/release/libdpdfnet_ladspa.so" "$P/lib/ladspa/libdpdfnet_ladspa.so"
mkdir -p "$P/lib/hushmic" "$P/share/hushmic/models" "$(dirname "$LOG")"
cp -a "$REPO"/assets/lib/libonnxruntime.so* "$P/lib/hushmic/"
install -m644 "$REPO"/assets/models/*.onnx "$REPO"/assets/models/*.weights.f32 "$P/share/hushmic/models/"

# Stop whatever HushMic runs now (AUR or local) so the new one takes the mic.
if pgrep -x hushmic >/dev/null; then
  "$P/bin/hushmic" quit >/dev/null 2>&1 || true
  for _ in {1..50}; do pgrep -x hushmic >/dev/null || break; sleep 0.1; done
  pgrep -x hushmic >/dev/null && { echo "hushmic did not quit; stop it and rerun" >&2; exit 1; }
fi

unset HUSHMIC_INFERENCE  # an inherited "onnx" would override auto
setsid -f "$P/bin/hushmic" --tray >"$LOG" 2>&1
echo "started $P/bin/hushmic --tray (log: $LOG)"

# The engine is reported once the plugin processes audio: hold the source open briefly to wake it.
sleep 1
timeout 6 pw-record --target hushmic_source /dev/null 2>/dev/null &
engine=""
for _ in {1..20}; do
  engine=$("$P/bin/hushmic" status --json 2>/dev/null | jq -r '.inference.engine // empty') || true
  [[ -n $engine ]] && break
  sleep 0.25
done
if [[ -z $engine ]]; then
  echo "running, but no engine reported within 5 s; check: $P/bin/hushmic status --json and $LOG" >&2
  exit 1
fi
if [[ $engine != "$WANT" ]]; then
  echo "WRONG ENGINE: $engine (wanted $WANT)" >&2
  "$P/bin/hushmic" status >&2 || true
  exit 1
fi
echo "ok: $engine"
"$P/bin/hushmic" status
