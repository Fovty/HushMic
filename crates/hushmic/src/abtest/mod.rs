//! Live A/B mic test window: side-by-side raw vs filtered spectrograms with
//! record/playback and honest summary metrics.
//!
//! Split: `types` is the backend↔UI contract, `state` the pure state
//! machine, `dsp` the FFT/level analysis, `metrics` the sample evaluation,
//! `audio` the PipeWire-facing backend thread, `ui` the egui layer. The UI
//! owns no audio state; everything crosses the two channels.

pub mod audio;
pub mod dsp;
pub mod metrics;
pub mod state;
pub mod stream;
pub mod types;
pub mod ui;

/// How the window ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEnd {
    Closed,
    /// pw-cat turned out too old for the live view once the device came
    /// up: the caller exits via `exit_needs_newer_pipewire`.
    NeedsNewerPipewire,
}

/// Run the window (blocking) until it is closed. `raw_node` is the physical
/// microphone feeding the chain (traced by the caller), `filtered_node` is
/// normally `hushmic_source`. `gate` is what blocked the mic test when a
/// plain launch opened the window anyway (open for a normal window).
pub fn run_window(
    raw_node: String,
    filtered_node: String,
    gate: types::LaunchGate,
) -> Result<WindowEnd, String> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<types::Command>();
    let (frame_tx, frame_rx) = std::sync::mpsc::channel::<types::Frame>();
    let backend = audio::Backend::new(raw_node, filtered_node, gate, cmd_rx, frame_tx);
    ui::run(backend, cmd_tx, frame_rx, gate)
}

/// pw-cat before the mid-2022 rework (Ubuntu 22.04 ships 0.3.48) cannot
/// stream a capture to a pipe — which is how the live view reads audio —
/// so the A/B window can only sit at −∞ there. Explain and exit 1: a
/// tray-requested window then falls back to the file-based recording test
/// (pw-cat writes a real file fine on every version), reusing the same
/// path as the no-GL fallback.
pub fn exit_needs_newer_pipewire() -> ! {
    use crate::notify::{self, Slot};
    eprintln!("hushmic: The live A/B view needs a newer PipeWire on this system.");
    // Bounded wait, not fire-and-forget: the detached send worker dies
    // with the process on the exit below and the notification would be
    // lost.
    notify::send_and_wait(
        Slot::MicTest,
        "audio-input-microphone",
        &crate::tr!("notify-mictest-title"),
        &crate::tr!("notify-old-pipewire-body"),
        std::time::Duration::from_secs(2),
    );
    std::process::exit(1);
}
