# Troubleshooting

Start with `hushmic --doctor`, or **Copy diagnostics** in Settings › About (Live shows the same button while the HushMic microphone is down). The report lists versions, install paths and assets, the PipeWire version and default source, the capture devices by name, the current settings, latency and quantum facts, and the last lines of the filter-chain log. It contains no audio. Read it over before pasting it into an [issue](https://github.com/Fovty/hushmic/issues) and add what you expected and what happened.

## No tray icon

GNOME has no tray of its own; install the *AppIndicator and KStatusNotifierItem Support* extension. HushMic runs without a tray and keeps trying to register the icon; launching it again opens the window, `hushmic status` shows what it is doing and `hushmic --headless` is the intended mode for tray-less setups. Cinnamon's tray sometimes appears a few seconds after login; HushMic waits for it.

## My app does not list HushMic

- PipeWire and WirePlumber must be running. PulseAudio-only apps (TeamSpeak, some Electron apps) also need `pipewire-pulse`.
- `hushmic mode` must not say `off`. `hushmic mode suppress` recreates the virtual mic.
- Some apps only show the system default input. Turn on **Use HushMic as the default microphone** in Settings › Apps.

## The recording still sounds noisy

The app is capturing the physical microphone, not HushMic. A few apps that use the Qt Multimedia backend (some KDE recorders) do this even when a virtual mic is selected; switch them to their PulseAudio or PipeWire backend. `hushmic status` names such an app after `direct:`. **Use HushMic as the default microphone** in Settings › Apps helps apps that follow the system default. **Replay the last 10 seconds** on Live lets you hear what HushMic itself produces.

## After suspend or a PipeWire restart

A watchdog recreates the virtual mic on its own, usually within seconds. If it keeps failing, the window says so, and you get a desktop notification unless the window has focus on a page that shows it (when notifications are on), and the doctor report shows why. `--enable-once` has no watchdog.

## No microphone after HushMic crashed

With **Use HushMic as the default microphone** on, HushMic makes its microphone the saved system default and gives the previous one back when it quits. A crash or a `kill -9` cannot do that, so the saved default still names HushMic's microphone, which is gone. Apps then fall back to some other input for the session, and the stale setting survives a reboot. The next start of HushMic repairs it (and says so), as does the uninstaller. To repair it without HushMic, clear the saved default and pick your microphone again in your sound settings:

```sh
pw-metadata -n default -d 0 default.configured.audio.source
```

## HushMic is silent, and a mixer lists "HushMic internal"

Mixers such as pavucontrol list two streams named **HushMic internal**: they carry HushMic's output to the HushMic microphone. Muting or turning them down there silences HushMic; HushMic puts them back to full volume each time it (re)starts, and `hushmic --doctor` points out a mute or a lowered volume. The **HushMic input (internal)** recording stream's volume is yours to set (to tame a loud mic); HushMic only undoes a mute on it.

## Another tool keeps re-routing the microphone

EasyEffects with *Process All Input Streams* adopts HushMic's capture stream, which silences the virtual mic. HushMic re-pins the stream, notifies you once and says so on Live; the fix is to disable that EasyEffects option or exclude `hushmic_input` in it.

## Latency and CPU

HushMic adds 80 ms: 10 ms of STFT framing, 40 ms of model context, and 30 ms of scheduling margin. Inference runs on its own thread, decoupled from the audio clock, so short CPU stalls do not chop the audio. PipeWire's own buffering comes on top. On PipeWire 1.6 and later the latency is reported to the graph, so apps that compensate (OBS) line the audio up automatically. While an app records from HushMic, its node sets `node.force-quantum = 480`, so the whole PipeWire graph runs in 10 ms periods, even when your `clock.min-quantum` is higher; the 80 ms are sized for that period. With nothing recording, the graph is yours again. A forced quantum above 480 samples increases the effective latency; `hushmic --doctor` shows the pin and reports that.

The models run on one of two engines. On CPUs with AVX2 and FMA (most Intel Core and AMD Ryzen CPUs since 2015; many older Pentium, Celeron and Atom chips lack them) HushMic uses a native engine with quantized matrix math (W7A8: 7-bit weights, 8-bit activations) that needs under half the CPU of ONNX Runtime, and about a quarter to a third on CPUs with AVX-VNNI. Elsewhere, or when its weight files are missing, it uses ONNX Runtime, where the quality model takes roughly a third of one desktop core at full clock speed and can need more than a whole core on a slow chip, so HushMic runs the light model there. Either costs more on a throttled laptop or when the other hyperthread of its core is busy. `hushmic status` names the engine (`engine: quality model (native w7a8 avx2)`; `avx-vnni` instead of `avx2` on CPUs with AVX-VNNI), and `hushmic --doctor` says why ONNX runs when it does. `hushmic config set inference onnx` keeps HushMic on ONNX Runtime; `auto` goes back to the default.

HushMic only does work while an app is reading audio from it. When nothing is, PipeWire pauses the filter chain and it uses no CPU; `hushmic status` then shows `chain: running, idle`. An app that keeps its microphone stream open while muted keeps it running (browser call apps do), and so does pavucontrol for its level meters while its window is open, and HushMic's own window while it shows Live (its recorder shows up in mixers and `pw-top` as "HushMic window (meter)"). PipeWire older than 0.3.68 (Ubuntu 22.04, Debian 12) also runs it while another app records the microphone directly. On any PipeWire version the same goes for a microphone that is itself a virtual source, such as a loopback or the output of PipeWire's echo-cancel module: while some other app reads from that source, HushMic's chain runs too.

When the CPU gets tight (a screen share, a video encoder) HushMic switches to the light model on its own, and past that to unfiltered audio; it tries the better model again a little later. A load that builds up gradually is caught before you can hear it and the switch is faded over. A sudden stall cannot be: the stream can lose a hop or two of audio and then runs unfiltered for about a tenth of a second, so you hear the room noise the model was removing, before a model takes over again. `hushmic status` shows which model is running (`engine: light model (fallback, native w7a8 avx2)` or `engine: passthrough (fallback)`), and the filter-chain log in the doctor report lists every switch with the measured cost. `hushmic config set model light` (or **Light** in Settings) makes the light model the default when that happens often.

Where the system allows it (user in the `audio` group, or rtkit or the desktop portal available) the inference thread runs with realtime priority so a busy browser cannot starve it; the log line `worker: realtime priority 10` in the doctor report confirms it, `not available` says why not. `HUSHMIC_WORKER_RT=0` turns the request off, `HUSHMIC_DSP_TIER=quality|light|passthrough` pins the engine (pinned to `quality`, the light model is not loaded), `HUSHMIC_VOICE_HOLD=1` turns on **Stay on my voice** in a host that is not HushMic, and `HUSHMIC_INFERENCE=auto|onnx|native` picks the inference engine, overriding the `inference` setting (set them in the environment HushMic starts from).

## Why does my desktop show the microphone in use when HushMic's window is open?

While the window shows Live, HushMic reads your microphone to draw it and to keep the last 10 seconds for Replay, so your desktop's microphone indicator lights up even when no app is using HushMic. Nothing is saved. It stops when you leave Live, minimise the window or close it.

## Cleaned files are FLAC, or a video lost its picture

Save writes the original's format, and keeps a video's picture, through the `ffmpeg` program. Without it, Save writes FLAC (a WAV stays WAV) and a video as its sound alone, in FLAC; with an `ffmpeg` that has no encoder for the original's codec, that file is saved as FLAC too. The `.deb` and the Fedora package recommend `ffmpeg`, so it comes along unless weak dependencies are turned off; the AUR packages list it as optional (`pacman -S ffmpeg`). With the install script or the AppImage, install your distribution's `ffmpeg` package (the install script says when it is missing). The Flatpak uses the one in its runtime; the codecs-extra extension adds AAC and MP3.

## Clean with HushMic does nothing

**Clean with HushMic** in the file manager (and `hushmic open-files`) hands the files to the running HushMic, and starts it when none runs. If it does not come up within 20 seconds, `hushmic open-files` says so, and what HushMic printed while starting is in `~/.local/state/hushmic/open-files.log` (in the Flatpak, `~/.var/app/io.github.fovty.HushMic/.local/state/hushmic/open-files.log`).

## The Flatpak saves into another folder

The Flatpak only sees the files you give it, so it cannot save next to them on its own. The first Save asks where cleaned files go, and they go there from then on; choose the recordings' own folder to keep each copy beside its original. To pick another folder, use **Change…** in **Settings › Files**.

## An app was linked into HushMic's microphone

An app's sound must never go into your microphone. Some setups name an output by its node number rather than its name (an older pipewire-pulse when you move an app in pavucontrol, or a tool started with `--target` and a number), and on WirePlumber 0.4 such an app follows that number to whatever node gets it next. When HushMic restarts, that can be HushMic's own microphone input. If an app's sound is linked into HushMic's microphone input anyway (by such a number, or by hand in a patchbay such as qpwgraph or Helvum), HushMic cuts that link within about a second and moves the app to your default output; the log says so. An app whose own settings keep linking it back is named in `hushmic status` (`kept off HushMic's microphone: …`) and in `hushmic --doctor`, and HushMic cuts its link each time it comes back: pick its output again by name in your sound settings, or restart the app. Recorders and level meters that listen to HushMic are left alone.

## Ubuntu 22.04

PipeWire 0.3.48 is supported by the native packages, with three limits: Live's replay can say it needs a newer PipeWire (`hushmic miccheck` runs an audio-only test instead), the Flatpak does not work there, and an app that records HushMic because you picked it by number (a tool started with `--target` and a number) stays on the microphone without noise suppression after you turn HushMic off and on: pick HushMic again for it. Moving it back by number would make WirePlumber 0.4.8 save HushMic as the microphone of every app of its kind. The apt conflict at install time is covered in [install.md](install.md#ubuntu-2204).
