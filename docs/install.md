# Installation notes

The [README](../README.md#install) has the install commands. This page covers the exceptions.

## Ubuntu 22.04

Stock 22.04 still ships `pipewire-media-session`, and apt refuses the deb with a conflict against `wireplumber`. Install both in one command; the trailing minus removes the old session manager:

```bash
sudo apt install ./hushmic_0.8.1-1_amd64.deb wireplumber pipewire-media-session-
```

Log out and back in afterwards. PipeWire 0.3.48 on 22.04 works with the native packages; the Flatpak does not support it.

## Install script prefixes

The script installs to `/usr` by default and needs root. Other prefixes:

```bash
# user-local, no root
curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sh -s -- --prefix "$HOME/.local"

# /usr/local
curl -fsSL https://raw.githubusercontent.com/Fovty/hushmic/main/scripts/install.sh | sudo sh -s -- --prefix /usr/local
```

A `$HOME/.local` install prints the environment variables the app needs; the autostart entry and the generated systemd unit carry them for you. The systemd unit file itself is installed only for `/usr` and `/usr/local`; other prefixes use `hushmic service install` (see [headless.md](headless.md)).

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

## Upgrading

Install the new package or AppImage over the old one, or run the install script again. The config in `~/.config/hushmic` is kept. If the systemd unit is enabled, restart it afterwards: `systemctl --user restart hushmic.service`.

## Uninstalling

Stop what runs first:

```bash
hushmic config set autostart false      # removes the login entry
systemctl --user disable --now hushmic.service   # if you enabled the unit
hushmic quit
```

Then remove the package with your package manager, run `hushmic-uninstall` for a script install (it remembers the prefix), or delete the AppImage. `hushmic service uninstall` removes a generated unit.
