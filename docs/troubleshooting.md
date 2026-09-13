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

HushMic adds 80 ms: 10 ms of STFT framing, 40 ms of model context, and 30 ms of scheduling margin. Inference runs on its own thread, decoupled from the audio clock, so short CPU stalls do not chop the audio; a sustained overload still can. PipeWire's own buffering comes on top. On PipeWire 1.6 and later the latency is reported to the graph, so apps that compensate (OBS) line the audio up automatically. A forced quantum above 480 samples increases the effective latency; the doctor reports it.

The quality model takes roughly a third of one desktop core (RTF about 0.3). `hushmic config set model dpdfnet2_48khz_hr` switches to the lighter model.

## Ubuntu 22.04

PipeWire 0.3.48 is supported by the native packages, with two limits: the live mic test window needs a newer PipeWire (an audio-only test runs instead), and the Flatpak does not work there. The apt conflict at install time is covered in [install.md](install.md#ubuntu-2204).
