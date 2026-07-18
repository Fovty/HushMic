# Running the hushmic DPDFNet filter-chain (operator notes)

Manual/operator steps to load the LADSPA plugin in PipeWire as a virtual
microphone source without the tray app, and to confirm it is visible via
pipewire-pulse and processes audio without xruns. The tray app manages this
lifecycle automatically (it hosts the filter-chain in a dedicated
`pipewire -c` child so it can inject the runtime env vars); these notes are
for running the chain by hand.

## Prerequisites

The release plugin and its runtime assets must be present on the target box:

```
~/hushmic-rt/lib/libdpdfnet_ladspa.so      # release build of dpdfnet-ladspa
~/hushmic-rt/lib/libonnxruntime.so         # bundled ONNX Runtime 1.27.x
~/hushmic-rt/models/dpdfnet8_48khz_hr.onnx # DPDFNet model (48 kHz, mono)
~/hushmic-rt/dpdfnet-mono.conf             # copy of live/dpdfnet-mono.conf
```

The plugin reads two env vars at runtime; they MUST be exported in the
environment of the process that hosts the filter-chain:

- `HUSHMIC_MODEL_PATH` -> the `.onnx` model to load
- `ORT_DYLIB_PATH`     -> the ONNX Runtime dylib (`ort` `load-dynamic`)

The absolute plugin path inside `dpdfnet-mono.conf` must match where the `.so`
actually lives (after a system install, `/usr/lib/ladspa/libdpdfnet_ladspa.so`).

## Load it (non-disruptive: dedicated host, default input untouched)

`dpdfnet-mono.conf` is a filter-chain *fragment* (only `context.modules =
[ filter-chain ]`), exactly like the stock `/usr/share/pipewire/filter-chain/`
examples. Run on its own it fails with `can't find protocol
'PipeWire:Protocol:Native'` because it carries none of the core modules.
PipeWire expects it to be *merged* with the base modules. Two working ways:

### A. conf.d drop-in (merges with the stock base) — recommended

```bash
mkdir -p ~/.config/pipewire/filter-chain.conf.d
cp live/dpdfnet-mono.conf ~/.config/pipewire/filter-chain.conf.d/hushmic.conf
export HUSHMIC_MODEL_PATH=$HOME/hushmic-rt/models/dpdfnet8_48khz_hr.onnx
export ORT_DYLIB_PATH=$HOME/hushmic-rt/lib/libonnxruntime.so
pipewire -c filter-chain.conf &   # base (/usr/share/pipewire) + the drop-in
sleep 2
# teardown also removes the drop-in:
#   rm ~/.config/pipewire/filter-chain.conf.d/hushmic.conf
```

### B. self-contained host (no edits to the user config dir)

Prepend the base modules to a copy of the fragment and run that single file:

```bash
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-rt              flags = [ ifexists nofail ] }
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-client-node     }
    { name = libpipewire-module-adapter         }
    # ... then the filter-chain block from live/dpdfnet-mono.conf verbatim ...
]
```

```bash
export HUSHMIC_MODEL_PATH=$HOME/hushmic-rt/models/dpdfnet8_48khz_hr.onnx
export ORT_DYLIB_PATH=$HOME/hushmic-rt/lib/libonnxruntime.so
pipewire -c ~/hushmic-rt/dpdfnet-mono-run.conf &   # dedicated host process
sleep 2
```

Either way the dedicated `pipewire -c` process connects to the running daemon
as a client and registers only the filter-chain nodes — it does not touch the
audio devices or the default source. Method B is what the tray app does.

## Verify the virtual source appears

```bash
wpctl status              | grep -i hushmic   # PipeWire graph
pactl list short sources  | grep -i hushmic   # pipewire-pulse visibility
```

A source must appear in **both**: `wpctl` shows the description "HushMic",
`pactl` lists the node name `hushmic_source`. The `pactl` listing is the
acceptance bar (it proves pipewire-pulse exposes the source to PulseAudio
clients). If it shows in `wpctl` but not `pactl`, check that
`playback.props.media.class = Audio/Source` in the config.

## Confirm it runs without xruns

```bash
pw-record --target "hushmic_source" /tmp/hushmic_cap.wav & sleep 6; kill %1
ls -lh /tmp/hushmic_cap.wav
journalctl --user -u pipewire --since "1 min ago" | grep -iE "xrun|underrun" \
  || echo "no xruns reported"
```

Expect a non-empty WAV and no xrun/underrun lines.

## RTF bench (re-confirm real-time budget)

```bash
cd ~/hushmic
HUSHMIC_MODEL_PATH=$HOME/hushmic-rt/models/dpdfnet8_48khz_hr.onnx \
ORT_DYLIB_PATH=$HOME/hushmic-rt/lib/libonnxruntime.so \
  cargo run --release --example bench_rtf -p dpdfnet-ladspa
```

Per-hop budget is 10 ms (480 samples @ 48 kHz). Target `RTF_p95 < 0.6`. If it
exceeds ~0.8, fall back to the lighter `dpdfnet2` model by pointing
`HUSHMIC_MODEL_PATH` at it (pure config swap, no rebuild).

## Tear down (leave the system exactly as found)

```bash
kill %1                      # the dedicated `pipewire -c` host
# (or) pw-cli destroy <module-id>
```

The dedicated host owns only the filter-chain module, so killing it removes the
virtual source and leaves the user's real default input unchanged.
