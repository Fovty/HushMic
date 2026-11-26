# Running without a tray icon

```bash
hushmic --headless
```

This runs everything the tray does (the virtual mic, the watchdog, microphone recovery, global shortcuts, the CLI socket) without a tray icon or a window. `hushmic status`, `mode`, `toggle`, `config` and `quit` control it. With `enabled = false` in the config it starts idle and prints how to turn the mic on (`hushmic mode suppress`).

`hushmic config set tray false` makes the normal launch and the autostart entry skip the icon as well. A plain `hushmic` launch still opens the window.

If no system tray exists (GNOME without the AppIndicator extension), HushMic keeps running, retries the icon in the background and says so once in a notification.

`--enable-once` is the older one-shot mode: it creates the virtual mic and waits for Ctrl+C, with no watchdog and no CLI socket. Use `--headless` unless a script needs exactly that.

## Starting at login

Use one of these, not both. Two instances at login race for the single-instance lock; the loser says "already running" and stops (started by systemd it exits cleanly, so the unit is not restarted; from a terminal or a script the exit code is 3).

### Desktop autostart entry

**Start HushMic when you log in** in Settings, or:

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

The status should say active. If it says inactive with "already running … this service instance stops" in its log, a tray instance was still running.

The unit starts before the desktop is up. The virtual mic works right away; global shortcuts wait until you are logged in to the desktop.

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

This writes `~/.config/systemd/user/hushmic.service` with the executable's path (and any `HUSHMIC_PLUGIN_SO`, `HUSHMIC_MODEL_DIR` or `ORT_DYLIB_PATH` you have set), reloads systemd, and turns the desktop autostart entry off. For an AppImage, run it through the AppImage file. If your install already ships a unit, the command tells you to enable that one instead. `hushmic service uninstall` disables and removes the generated unit.

The generated unit records the executable path. If you move the AppImage or the install, run `service install` again.

### Flatpak

The Flatpak cannot write host units. Create `~/.config/systemd/user/hushmic.service` on the host yourself. Stopping goes through `hushmic quit` because the unit's main process is the sandbox launcher, not HushMic. This unit starts with the graphical session, not at `default.target`: from inside the sandbox HushMic cannot tell when the desktop is up, and asking the desktop portal for shortcuts before that breaks the portal for the whole session:

```ini
[Unit]
Description=HushMic noise-suppression virtual microphone
After=graphical-session.target pipewire.service wireplumber.service
Wants=pipewire.service
PartOf=graphical-session.target

[Service]
ExecStart=flatpak run io.github.fovty.HushMic --headless
ExecStop=flatpak run --command=hushmic io.github.fovty.HushMic quit
Restart=on-failure
RestartPreventExitStatus=3
RestartSec=3
TimeoutStopSec=30

[Install]
WantedBy=graphical-session.target
```

Then `systemctl --user daemon-reload` and `systemctl --user enable --now hushmic.service`. Turn the Flatpak's own autostart off first (`flatpak run io.github.fovty.HushMic config set autostart false`). This needs a Flatpak built from a release with headless support. Desktops that do not start `graphical-session.target` (Cinnamon, Xfce, MATE, LXQt) never start this unit; use the autostart entry there.

## Push to talk in sway, i3 and other window managers

Without the desktop's shortcut support, push to talk needs a key that does one thing when pressed and another when released. sway and i3 bind those separately:

```text
# ~/.config/sway/config; in i3 leave out --no-repeat (see below)
bindsym --no-repeat Pause exec hushmic mode suppress
bindsym --release   Pause exec hushmic mode mute
```

Use a single key without modifiers, such as Pause or F13: a release binding matches the modifiers still held when the key comes up, so with a chord like Mod4+grave, letting go of Mod4 first skips the release binding and leaves the mic live. In i3 (X11) a held key repeats as release and press pairs; turn repeat off for that key with `xset -r 127` (127 is Pause; `xev` shows another key's code). Start muted (`hushmic mode mute`), hold the key to talk. For push to mute, swap the two modes (or use `bypass` in place of `suppress` to talk unfiltered). A single toggle key needs no release binding: `bindsym Mod4+m exec hushmic toggle mute`. Each `hushmic mode` call is a short request to the running instance, fast enough for a key press.
