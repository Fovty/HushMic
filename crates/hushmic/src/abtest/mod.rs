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

/// Run the window (blocking) until it is closed. `raw_node` is the physical
/// microphone feeding the chain (traced by the caller), `filtered_node` is
/// normally `hushmic_source`.
pub fn run_window(raw_node: String, filtered_node: String) -> Result<(), String> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<types::Command>();
    let (frame_tx, frame_rx) = std::sync::mpsc::channel::<types::Frame>();
    let backend = audio::Backend::new(raw_node, filtered_node, cmd_rx, frame_tx);
    ui::run(backend, cmd_tx, frame_rx)
}
