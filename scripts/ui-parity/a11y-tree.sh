#!/bin/bash
# The AT-SPI tree of one ui-shot scene: a private session bus with the
# accessibility bus switched on, the scene held open, the tree printed.
#   scripts/ui-parity/a11y-tree.sh SCENE THEME WxH [key=value …]
# Needs xvfb-run, dbus-run-session, at-spi2-core and python3-gi (Atspi);
# build the example first: cargo build -p hushmic --example ui-shot
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
bin=${UI_SHOT:-$here/../../target/debug/examples/ui-shot}
launcher=
for l in /usr/libexec/at-spi-bus-launcher /usr/lib/at-spi2-core/at-spi-bus-launcher; do
    [ -x "$l" ] && launcher=$l && break
done
[ -x "$bin" ] || { echo "no $bin (build the ui-shot example)" >&2; exit 1; }
[ -n "$launcher" ] || { echo "no at-spi-bus-launcher (at-spi2-core)" >&2; exit 1; }
scene=$1 theme=$2 size=$3
shift 3
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
export bin launcher scene theme size out here
export params="$*"
xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- bash -c '
    "$launcher" --launch-immediately >/dev/null 2>&1 &
    sleep 1
    gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
        --method org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled "<true>" >/dev/null
    "$bin" --hold 6 "$scene" "$theme" "$size" "$out/shot.png" $params >/dev/null 2>"$out/err" &
    pid=$!
    sleep 4
    python3 -I "$here/a11y-tree.py" "$(basename "$bin")" 2>/dev/null
    wait $pid || { cat "$out/err" >&2; exit 1; }
    if [ -s "$out/err" ]; then cat "$out/err" >&2; fi
' 2>&1 | grep -vE 'dbus-daemon|SpiRegistry|dbind-WARNING|^$'
