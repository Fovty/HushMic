# HushMic as a Flatpak

The manifest, PipeWire compat patch, Manager-category client drop-in,
AppStream metainfo, and generated crate list. Intended for
immutable/Flatpak-first systems; the native packages remain the recommended
install elsewhere.

## Build

Needs `flatpak-builder` >= 1.4 (older versions fail at export against the
25.08 runtime; on Debian 12 it's in backports). The manifest builds the
**pinned release tag** — to build your working tree instead, swap the
`hushmic` module's source for the `type: dir` variant shown in the
manifest comment.

```bash
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install-deps-from=flathub --force-clean --install \
    build-dir packaging/flatpak/io.github.fovty.HushMic.yml
flatpak run io.github.fovty.HushMic
```

Bundle for another machine: add `--repo=repo`, then
`flatpak build-bundle repo hushmic.flatpak io.github.fovty.HushMic`.
After a `Cargo.lock` change, regenerate `cargo-sources.json` with
[flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo),
or offline with `python3 scripts/cargo-sources.py Cargo.lock packaging/flatpak/cargo-sources.json`
(same output while every crate comes from crates.io).
After manifest/metainfo changes, re-run the linters:
`flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest|appstream <file>`.

## How the sandboxed app works

Same architecture as native: the tray spawns two bundled `pipewire -c`
children hosting `module-filter-chain` (the DSP chain, and a small host
that carries `hushmic_source` through chain restarts), which talk to the
**host** daemon over the shared `pipewire-0` socket; host WirePlumber links
the mic in and exposes `hushmic_source` to every app, and the daemon links
the chain into the source host with the bundled `pw-link`. The manifest builds PipeWire from source
(the runtime ships no daemon or filter-chain module), ONNX Runtime from
source, and installs a `media.category = Manager` client drop-in so the
bundled `pw-metadata` may set the default mic (WirePlumber silently drops
metadata writes from ordinary flatpak clients). Each sandbox-specific
behavior is documented at its call site: SNI naming and exported tray icons
(`tray.rs`, `main.rs`), Background-portal autostart (`portal.rs`), host
version probing and default-source read-back (`pipewire.rs`), the
cross-instance lock/show-socket in `$XDG_RUNTIME_DIR/app/$FLATPAK_ID`
(`lock.rs`).

"Clean a file" needs nothing beyond the runtime: Opus decodes through its
libopus, Save writes through its `/usr/bin/ffmpeg` (the codecs-extra
extension adds AAC and MP3; without an encoder for the original's codec,
Save writes FLAC), the file workers' `SCHED_IDLE` holds inside the
sandbox, and the files come and go through the FileChooser portal
(`files/chooser.rs`). A sandbox sees only the files it was given, so
cleaned files go into a folder the user chooses once (`files_folder` in
the config) rather than next to the original. "Show in folder" uses the
OpenURI portal there (no talk name for the file manager).

## Host compatibility

- **Host floor: PipeWire ≥ 0.3.65** (Debian 12). On 0.3.48 (Ubuntu 22.04)
  the chain does not process; use the native packages there.
- `pipewire-remote-node-old-server-compat.patch` fixes three stacked
  version-skew gaps that leave capture graphs silently dead when a ≥ 0.3.74
  client meets a < 0.3.72 daemon: restored on-demand mix creation in
  remote-node (removed upstream in `a9a9c72a0` after `1ce94628e`), and
  `clock.target_rate`/`target_duration` fallbacks in audioconvert and
  module-filter-chain (pre-0.3.7x drivers never fill those fields).
  Verified with a real-speech DSP-flow test on Debian 12 and Fedora 44
  (output level matches the native baseline). Not yet reported upstream;
  filing it is the path to dropping the patch.
- Old hosts' `pw-dump` (< ~0.3.80) emits invalid JSON whenever a modern
  client's node is in the graph; HushMic repairs it app-side.

## Limitations

- GNOME needs the AppIndicator extension for the tray icon (without it
  no `StatusNotifierWatcher` exists on the bus). Since 0.8.0 the app keeps
  running without an icon and says so in a notification; earlier releases
  exit after 60 s.
- Don't run a native install and the Flatpak simultaneously — their
  single-instance locks live in different places and both would fight over
  `hushmic_source`.
- On hosts whose WirePlumber lacks Manager-category support, "Set as
  default microphone" fails loudly (write-then-read-back) instead of
  silently.
