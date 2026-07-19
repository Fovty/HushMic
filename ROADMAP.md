# Roadmap

The order below is directional; features
are not tied to specific releases yet.

## Next

- **Seamless bypass and mute**: keep the HushMic virtual device present while
  passing through raw audio or outputting silence.
- **CLI and shortcut controls**: expose bypass, mute, and status commands for
  desktop shortcuts and integrations; explore global shortcuts and
  push-to-talk where supported.

## Shipped

- **v0.4.0**: better microphone recovery, per-microphone preferences, and
  built-in diagnostics (`hushmic --doctor`).

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
