# Roadmap

The order below is directional; features
are not tied to specific releases yet.

## Next

- Nothing locked in currently

## Shipped

- **v0.5.0**: seamless bypass and mute, a scriptable CLI
  (`hushmic status | mode | toggle`), global keyboard shortcuts with
  push-to-talk and push-to-mute (desktop portal), and accurate latency
  reporting to PipeWire.
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
