# Troubleshooting

Start with `hushmic --doctor`, or **Copy diagnostics** in the About window. The report lists versions, install paths and assets, the PipeWire version and default source, the capture devices by name, the current settings, latency and quantum facts, and the last lines of the filter-chain log. It contains no audio. Read it over before pasting it into an [issue](https://github.com/Fovty/hushmic/issues) and add what you expected and what happened.

## No tray icon

GNOME has no tray of its own; install the *AppIndicator and KStatusNotifierItem Support* extension. HushMic runs without a tray and keeps trying to register the icon; `hushmic status` shows what it is doing and `hushmic --headless` is the intended mode for tray-less setups. Cinnamon's tray sometimes appears a few seconds after login; HushMic waits for it.

## My app does not list HushMic

- PipeWire and WirePlumber must be running. PulseAudio-only apps (TeamSpeak, some Electron apps) also need `pipewire-pulse`.
- `hushmic mode` must not say `off`. `hushmic mode suppress` recreates the virtual mic.
- Some apps only show the system default input. Turn on **Set as default microphone**.

## The recording still sounds noisy

The app is capturing the physical microphone, not HushMic. A few apps that use the Qt Multimedia backend (some KDE recorders) do this even when a virtual mic is selected; switch them to their PulseAudio or PipeWire backend. **Set as default microphone** helps apps that follow the system default. **Test my mic** shows what HushMic itself produces.

## After suspend or a PipeWire restart

A watchdog recreates the virtual mic on its own, usually within seconds. If it keeps failing you get a desktop notification (when notifications are on) and the doctor report shows why. `--enable-once` has no watchdog.

## Another tool keeps re-routing the microphone

EasyEffects with *Process All Input Streams* adopts HushMic's capture stream, which silences the virtual mic. HushMic re-pins the stream and notifies you once; the fix is to disable that EasyEffects option or exclude `hushmic_input` in it.

## Latency and CPU

HushMic adds 80 ms: 10 ms of STFT framing, 40 ms of model context, and 30 ms of scheduling margin. Inference runs on its own thread, decoupled from the audio clock, so short CPU stalls do not chop the audio. PipeWire's own buffering comes on top. On PipeWire 1.6 and later the latency is reported to the graph, so apps that compensate (OBS) line the audio up automatically. A forced quantum above 480 samples increases the effective latency; the doctor reports it.

The quality model takes roughly a third of one desktop core at full clock speed, more on a throttled laptop or when the other hyperthread of its core is busy. When the CPU gets tight (a screen share, a video encoder) HushMic switches to the light model on its own, and past that to unfiltered audio; it tries the better model again a little later. A load that builds up gradually is caught before you can hear it and the switch is faded over. A sudden stall cannot be: the stream can lose a hop or two of audio and then runs unfiltered for about a tenth of a second, so you hear the room noise the model was removing, before a model takes over again. `hushmic status` shows which engine is running (`engine: light model (fallback)` or `engine: passthrough (fallback)`), and the filter-chain log in the doctor report lists every switch with the measured cost. `hushmic config set model dpdfnet2_48khz_hr` makes the light model the default when that happens often.

Where the system allows it (user in the `audio` group, or rtkit or the desktop portal available) the inference thread runs with realtime priority so a busy browser cannot starve it; the log line `worker: realtime priority 10` in the doctor report confirms it, `not available` says why not. `HUSHMIC_WORKER_RT=0` turns the request off, `HUSHMIC_DSP_TIER=quality|light|passthrough` pins the engine (set them in the environment HushMic starts from).

## Ubuntu 22.04

PipeWire 0.3.48 is supported by the native packages, with two limits: the live mic test window needs a newer PipeWire (an audio-only test runs instead), and the Flatpak does not work there. The apt conflict at install time is covered in [install.md](install.md#ubuntu-2204).
