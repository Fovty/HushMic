# Running without a tray icon

```bash
hushmic --headless
```

This runs everything the tray does (the virtual mic, the watchdog, microphone recovery, global shortcuts, the CLI socket) without a tray icon or a window. `hushmic status`, `mode`, `toggle`, `config` and `quit` control it. With `enabled = false` in the config it starts idle and prints how to turn the mic on (`hushmic mode suppress`).

`hushmic config set tray false` makes the normal launch and the autostart entry skip the icon as well. A plain `hushmic` launch still opens the mic test window.

If no system tray exists (GNOME without the AppIndicator extension), HushMic keeps running, retries the icon in the background and says so once in a notification.

`--enable-once` is the older one-shot mode: it creates the virtual mic and waits for Ctrl+C, with no watchdog and no CLI socket. Use `--headless` unless a script needs exactly that.

## Starting at login

Use one of these, not both. Two instances at login race for the single-instance lock; the loser exits with "already running".

### Desktop autostart entry

**Start on login** in the tray menu, or:

```bash
hushmic config set autostart true
```

This writes `~/.config/autostart/hushmic.desktop`, which starts `hushmic --tray`. Combine it with `tray = false` for a silent start on a desktop without a tray.

### systemd user unit

The deb, rpm, AUR packages and the tarball (for `/usr` and `/usr/local`) ship `hushmic.service`, which runs `hushmic --headless`:

```bash
hushmic config set autostart false      # if the entry was on
hushmic quit                            # if a tray instance runs
systemctl --user enable --now hushmic.service
systemctl --user status hushmic.service
```

The status should say active. If it exited at once, a tray instance was still running.

To start at boot before you log in:

```bash
loginctl enable-linger
```

The microphone device may only become accessible after your first graphical login (device permissions); HushMic picks it up on its own.

Before uninstalling the package, run `systemctl --user disable --now hushmic.service`.

### AppImage, home prefixes and Nix

These installs have no unit in a place systemd reads. Generate one for the installed path:

```bash
hushmic service install
systemctl --user enable --now hushmic.service
```

This writes `~/.config/systemd/user/hushmic.service` with the right executable path and environment, reloads systemd, and turns the desktop autostart entry off. For an AppImage, run it through the AppImage file. If your install already ships a unit, the command tells you to enable that one instead. `hushmic service uninstall` disables and removes the generated unit.

The generated unit records the executable path. If you move the AppImage or the install, run `service install` again.

### Flatpak

The Flatpak cannot write host units. Create `~/.config/systemd/user/hushmic.service` on the host yourself. Stopping goes through `hushmic quit` because the unit's main process is the sandbox launcher, not HushMic:

```ini
[Unit]
Description=HushMic noise-suppression virtual microphone
After=pipewire.service wireplumber.service
Wants=pipewire.service

[Service]
ExecStart=flatpak run io.github.fovty.HushMic --headless
ExecStop=flatpak run --command=hushmic io.github.fovty.HushMic quit
Restart=on-failure
RestartSec=3
TimeoutStopSec=10

[Install]
WantedBy=default.target
```

Then `systemctl --user daemon-reload` and `systemctl --user enable --now hushmic.service`. Turn the Flatpak's own autostart off first (`flatpak run io.github.fovty.HushMic config set autostart false`). This needs a Flatpak built from a release with headless support.
