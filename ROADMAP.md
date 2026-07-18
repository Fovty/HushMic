# Roadmap

The order below is directional; features
are not tied to specific releases yet.

## Next

- **Better microphone recovery**: temporarily follow the system default when
  the selected microphone disappears, then switch back when it returns.
- **Built-in diagnostics**: add `hushmic --doctor` and a copyable diagnostic
  report.

## Planned

- **Seamless bypass and mute**: keep the HushMic virtual device present while
  passing through raw audio or outputting silence.
- **CLI and shortcut controls**: expose bypass, mute, and status commands for
  desktop shortcuts and integrations; explore global shortcuts and
  push-to-talk where supported.
- **Per-microphone preferences**: remember the chosen model and suppression
  strength for each microphone without adding a profile editor.

## Exploring

- **Incoming voice cleanup**: an optional virtual output for cleaning voice
  chat from applications such as Discord and TeamSpeak. This will only move
  forward if latency, CPU usage, stereo handling, routing, and Flatpak support
  are satisfactory.

## Out of scope

- A full effects rack or routing matrix
- Cloud processing, telemetry, or automatic model downloads
- GPU acceleration
- Windows or macOS ports
- Additional plugin formats unless there is clear demand
