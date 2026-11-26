# Installation notes

The [README](../README.md#install) has the install commands. This page covers the exceptions.

## Debian and Ubuntu

Double-clicking the downloaded `.deb` opens it in the software center on most desktops. If the installer window does nothing (on Kubuntu it can sit there), use the two commands from the README instead. apt may end with a line starting `N: Download is performed unsandboxed as root`; that is harmless, the package is installed.

## Ubuntu 22.04

Stock 22.04 still ships `pipewire-media-session`, and apt refuses the deb with a conflict against `wireplumber`. Install both in one command; the trailing minus removes the old session manager:

```bash
sudo apt install ./hushmic_amd64.deb wireplumber pipewire-media-session-
```

The desktop keeps PulseAudio as its sound server after that. Switch your session to PipeWire, as yourself (not with sudo), then log out and back in:

```bash
systemctl --user disable --now pulseaudio.service pulseaudio.socket
systemctl --user enable --now pipewire.socket pipewire-pulse.socket wireplumber.service
```

PipeWire 0.3.48 on 22.04 works with the native packages; the Flatpak does not support it.

## The sound system check

Before it installs anything, the install script checks that PipeWire with its command line tools, WirePlumber and pipewire-pulse are installed and running in your session, and that PulseAudio is not the sound server instead. Where something is missing, it prints the install command for your distribution (apt, dnf, pacman or zypper) and the `systemctl --user` commands that start PipeWire, then asks whether to install HushMic anyway; it never installs packages or changes services itself. `--check` runs only the check, and `--no-check` skips it (for scripts; without a terminal to ask, the script says what is missing and goes on). Under `sudo` it reads your session; as root without it (`su -`, `sudo su -`) it finds you through `logname` or pkexec, and where it can't, it checks only what is installed and goes on without asking. The .deb recommends `pipewire-audio` where the distribution has it (Debian 12 and 13, Ubuntu 24.04, Mint 22), which takes PulseAudio's place; apt lists that before it asks.

## Install script prefixes

The script installs to `/usr` by default. It uses `sudo` only where your user cannot write (and says so first: `using sudo for /usr (/usr is not writable)`), so a prefix you own, in your home or elsewhere, needs no root. Other prefixes:

```bash
# user-local, no root
curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sh -s -- --prefix "$HOME/.local"

# /usr/local
curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sudo sh -s -- --prefix /usr/local
```

Any prefix works without environment variables: the binary finds its plugin, models and ONNX Runtime next to itself (`hushmic --version` prints where). The script says so if it finds them missing, and reminds you when the prefix's `bin` is not on your `PATH`. If you set `HUSHMIC_PLUGIN_SO`, `HUSHMIC_MODEL_DIR` or `ORT_DYLIB_PATH` yourself, the autostart entry and a generated systemd unit keep them. The systemd unit file itself is installed only for `/usr` and `/usr/local`; for other prefixes the script prints the next step, `hushmic service install` (see [headless.md](headless.md)).

On an immutable system with a read-only `/usr` (Silverblue, Kinoite, Bazzite, SteamOS) use the AppImage, a home prefix, or the Flatpak.

## Arch Linux

`hushmic-bin` installs the release build. The `hushmic` package builds from the tagged source against the system `onnxruntime` package. `paru` works the same as `yay`.

## NixOS

The flake lives in [Fovty/hushmic-nix](https://github.com/Fovty/hushmic-nix) and builds from source. To keep it installed:

```bash
nix profile install github:Fovty/hushmic-nix
```

If Nix complains about experimental features, prepend `--extra-experimental-features 'nix-command flakes'`.

## AppImage

Keep the file at a stable path before turning on autostart or generating a systemd unit; both record the path. CLI commands go through the same file:

```bash
./hushmic-x86_64.AppImage status
./hushmic-x86_64.AppImage config set attn_limit strong
```

## Flatpak

There is no Flathub listing yet. [packaging/flatpak/README.md](../packaging/flatpak/README.md) explains how to build and install the manifest with `flatpak-builder`. The host needs PipeWire 0.3.65 or newer. Do not run a native install and the Flatpak at the same time; both would create `hushmic_source`.

## Building from source

```bash
git clone https://github.com/Fovty/hushmic
cd hushmic
./scripts/setup-assets.sh        # fetch the DPDFNet models, native weights and ONNX Runtime
cargo build --release
```

This produces `target/release/hushmic` and `target/release/libdpdfnet_ladspa.so`. `scripts/install.sh` shows the install layout; `crates/dpdfnet-ladspa/examples/run-filter-chain.md` shows how to load the plugin by hand.

A plain `cargo build` opens Opus recordings (Ogg, WebM, MKV) only through `ffmpeg`. The packages build with `--features hushmic/opus`, which decodes them through libopus (install its development package, `libopus-dev` or `opus-devel`, to build with it); the release builds (tarball, `.deb`, AppImage, `hushmic-bin`) link it in statically (`OPUS_LIB_STATIC=true`, with `OPUS_LIB_DIR` set to the folder that holds `libopus.a`), so they need no libopus at run time. Saving in the original's format and keeping a video's picture need the `ffmpeg` program at run time; without it, cleaned files are saved as FLAC (a WAV stays WAV), and videos as their sound only.

## Upgrading

Install the new package or AppImage over the old one, or run the install script again. The config in `~/.config/hushmic` is kept. If the systemd unit is enabled, restart it afterwards: `systemctl --user restart hushmic.service`.

What HushMic generates and nobody edits (the two PipeWire configs it runs, the note of your previous default microphone, the filter-chain log) lives in `~/.local/state/hushmic` (`$XDG_STATE_HOME`). Versions before 1.0 wrote the configs into `~/.config/hushmic`; the first start moves them.

## Uninstalling

Stop what runs first:

```bash
hushmic config set autostart false      # removes the login entry
systemctl --user disable --now hushmic.service   # if you enabled the unit
hushmic quit
```

Then remove the package with your package manager, run `hushmic-uninstall` for a script install (it remembers the prefix), or delete the AppImage. `hushmic service uninstall` removes a generated unit; `hushmic-uninstall` removes one too when it runs that install's binary (and names one that runs another), and leaves no empty folders of its own in the prefix.

`hushmic-uninstall` also stops a running HushMic, puts your default microphone back if a crash left it on HushMic's (otherwise apps that follow the default would find no microphone), and removes `~/.local/state/hushmic` and `~/.cache/hushmic`. After a package removal, or for the AppImage, do that by hand: `hushmic quit` before removing it gives the default back, and the two folders can go. Your settings in `~/.config/hushmic` stay in every case; delete that folder to forget them.
