<h1 align="center">
  <img src="docs/img/hushmic-logo.png" alt="HushMic" width="120"><br>
  HushMic
</h1>

<p align="center">
  <b>Real-time microphone noise suppression for Linux, as a system-wide virtual mic.</b><br>
  Open source, runs on the CPU, your audio never leaves the machine.
</p>

<p align="center">
  <a href="https://github.com/Fovty/hushmic/releases"><img src="https://img.shields.io/github/v/release/Fovty/hushmic?sort=semver" alt="Release"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="License"></a>
  <a href="https://github.com/Fovty/hushmic/actions/workflows/release.yml"><img src="https://github.com/Fovty/hushmic/actions/workflows/release.yml/badge.svg" alt="Build"></a>
  <a href="https://ko-fi.com/fovty"><img src="https://img.shields.io/badge/Ko--fi-support-ff5e5b?logo=ko-fi&logoColor=white" alt="Support on Ko-fi"></a>
</p>

HushMic adds a virtual microphone to PipeWire that removes keyboard noise, fans and background chatter from your voice in real time. Pick **HushMic** as the input in Discord, TeamSpeak, OBS or your browser. The model is DPDFNet, which scored above Krisp, DeepFilterNet and RNNoise on the demo clips (see [Why DPDFNet](#why-dpdfnet)). Processing adds 80 ms of latency.

## Demo

Each clip plays the noisy input, then the same audio cleaned by HushMic.

<table>
<tr>
<td align="center"><b>Keyboard</b></td>
<td align="center"><b>Fan / AC hum</b></td>
<td align="center"><b>Café chatter</b></td>
</tr>
<tr>
<td><video src="https://github.com/user-attachments/assets/9cc7abe4-26fc-440f-88da-b86ef33df142" controls width="280"></video></td>
<td><video src="https://github.com/user-attachments/assets/d4b396a7-d980-4e0d-bac2-2a119debd768" controls width="280"></video></td>
<td><video src="https://github.com/user-attachments/assets/f4cd4504-d49a-4a46-95c2-01e56cd2d268" controls width="280"></video></td>
</tr>
</table>

Audio sources and licenses: [docs/demo/ASSETS.md](docs/demo/ASSETS.md). Measurements: [docs/comparison.md](docs/comparison.md).

## Install

Requirements: Linux on x86-64 with PipeWire and WirePlumber. PulseAudio apps need `pipewire-pulse`. A tray icon needs StatusNotifierItem support, which GNOME gets from the *AppIndicator and KStatusNotifierItem Support* extension. Without a tray, HushMic still runs and the command line controls it.

**Install script** (any distro, system-wide):

```bash
curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sudo sh
```

**Debian / Ubuntu**:

```bash
curl -fsSLO https://github.com/Fovty/hushmic/releases/latest/download/hushmic_0.9.1-1_amd64.deb
sudo apt install ./hushmic_0.9.1-1_amd64.deb
```

**Arch Linux** (AUR):

```bash
yay -S hushmic-bin
```

**Fedora** (COPR):

```bash
sudo dnf copr enable fovty/hushmic
sudo dnf install hushmic
```

**NixOS** (flake, builds from source):

```bash
nix run github:Fovty/hushmic-nix -- --tray
```

**AppImage**:

```bash
curl -fsSLO https://github.com/Fovty/hushmic/releases/latest/download/hushmic-x86_64.AppImage
chmod +x hushmic-x86_64.AppImage
./hushmic-x86_64.AppImage --tray
```

Ubuntu 22.04, custom prefixes, Nix flags, Flatpak and uninstalling: [docs/install.md](docs/install.md).

## Usage

Start HushMic from your application menu. A tray icon appears and noise suppression is on. Pick your microphone in the tray menu, then choose **HushMic** as the input in your app, or turn on **Set as default microphone** so apps that follow the system default use it.

The menu switches between noise suppression, **bypass** (your raw voice) and **mute** (silence on the virtual mic). Switching is instant and does not reconnect your call. It also holds the model choice (quality or light), the suppression strength, start on login and global shortcuts.

**Test my mic** opens a window with the raw microphone and the cleaned output side by side, and records a 10-second sample you can play back both ways.

<p align="center">
  <img src="docs/img/hushmic-ab-window.png" alt="Live A/B mic test" width="720">
</p>

The same controls work from a terminal, so any hotkey tool can drive them:

```bash
hushmic status              # what the running instance is doing
hushmic mode mute           # suppress | bypass | mute | off
hushmic toggle mute         # one key on, same key off
hushmic config set attn_limit strong
hushmic quit
```

Menu screenshots, keyboard shortcuts, every command and every config key: [docs/usage.md](docs/usage.md).

## Starting at login

Turn on **Start on login** in the tray menu, or run `hushmic config set autostart true`. For a daemon without a tray icon, `hushmic --headless` and the systemd user unit are described in [docs/headless.md](docs/headless.md). Use one of the two, not both.

## Why DPDFNet

I wanted noise suppression on Linux that matches Krisp and installs like an app. I ran the three demo clips through the candidates and scored the output with DNSMOS P.835 (1 to 5, higher is better):

| Model                             | Overall | Background | Speech |
| --------------------------------- | :-----: | :--------: | :----: |
| **DPDFNet** (HushMic)             | **3.20** | **4.15**  | **3.43** |
| DeepFilterNet 3                   |  2.97   |    3.98    |  3.26  |
| Krisp v9.9.3                      |  2.57   |    3.96    |  2.81  |
| khip (older Krisp model port)     |  2.43   |    3.78    |  2.71  |
| GTCRN                             |  2.42   |    3.71    |  2.76  |
| RNNoise (EasyEffects default)     |  2.01   |    3.93    |  2.60  |
| Unprocessed input                 |  1.49   |    1.48    |  2.01  |

Three clips are not a formal benchmark, but the source audio is public. Method, per-clip numbers and CPU cost: [docs/comparison.md](docs/comparison.md). DPDFNet is a DeepFilterNet-lineage model by Ceva ([paper](https://arxiv.org/abs/2512.16420)).

## How it works

- [`hushmic-denoiser`](crates/hushmic-denoiser) runs the DPDFNet ONNX model on 48 kHz mono audio. It is a plain Rust library you can embed in your own app.
- `dpdfnet-ladspa` wraps it as a LADSPA plugin.
- `hushmic` writes a PipeWire `module-filter-chain` config and runs it as a child process. PipeWire owns the real-time scheduling, so no `setcap` is needed. A watchdog recreates the virtual mic after a PipeWire restart or suspend, and quitting restores your previous default input.

## FAQ

**Does my audio go anywhere?** No. Everything runs on the CPU and nothing is uploaded.

**How much latency and CPU?** 80 ms of processing (10 ms framing, 40 ms model context, 30 ms scheduling margin) plus PipeWire's own buffering. On PipeWire 1.6 and later the latency is reported to the graph, so apps like OBS can compensate. The quality model takes about a third of one core; the light model is cheaper.

**No tray icon on GNOME?** Install the *AppIndicator and KStatusNotifierItem Support* extension. HushMic keeps running without it.

More: [docs/troubleshooting.md](docs/troubleshooting.md).

## Alternatives

- [NoiseTorch-ng](https://github.com/noisetorch/NoiseTorch): RNNoise-based virtual mic, needs `setcap`.
- [EasyEffects](https://github.com/wwmm/easyeffects): full PipeWire effects suite with an RNNoise denoiser.
- [noise-suppression-for-voice](https://github.com/werman/noise-suppression-for-voice): RNNoise LADSPA/VST plugin, wired by hand.
- [DeepFilterNet](https://github.com/Rikorose/DeepFilterNet): the model family HushMic builds on, with its own LADSPA plugin.

## Build from source

```bash
git clone https://github.com/Fovty/hushmic
cd hushmic
./scripts/setup-assets.sh        # fetch the DPDFNet models and ONNX Runtime
cargo build --release
```

This produces `target/release/hushmic` and `target/release/libdpdfnet_ladspa.so`. `scripts/install.sh` shows the install layout; `crates/dpdfnet-ladspa/examples/run-filter-chain.md` shows how to load the plugin by hand.

## Contributing

Translations happen on [Weblate](https://hosted.weblate.org/projects/hushmic/), see [TRANSLATING.md](TRANSLATING.md). Plans are in the [roadmap](ROADMAP.md). If HushMic is useful to you, a coffee on [Ko-fi](https://ko-fi.com/fovty) helps keep it maintained.

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)), at your option.

## Credits

[DPDFNet](https://github.com/ceva-ip/DPDFNet) by Ceva (Apache-2.0) is the speech-enhancement model. The real-time plugin follows the architecture of [DeepFilterNet](https://github.com/Rikorose/DeepFilterNet). Built with [PipeWire](https://pipewire.org), [ort](https://github.com/pykeio/ort), [rustfft](https://github.com/ejmahler/RustFFT) and [ksni](https://github.com/iovxw/ksni). Demo voice: "After Love" by Sara Teasdale, read by a LibriVox volunteer (public domain); keyboard by C40115 and fan hum by Gravity Sound (CC BY 4.0); café ambience public domain. Details in [docs/demo/ASSETS.md](docs/demo/ASSETS.md).
