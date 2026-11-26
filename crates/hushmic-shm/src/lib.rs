//! Shared-memory layouts between the HushMic plugin and the app.
//!
//! - [`meter`]: the engine meter (per 10 ms hop, the level going into the
//!   engine and the level leaving it), one page the plugin's DSP worker
//!   writes and the app maps read-only.
//! - [`tap`]: the Mic check tap (the raw and the clean audio of the same
//!   hops), a ring that exists only while the Mic check page is visible.
//! - [`control`]: the mode and strength the app wants, one page the app
//!   writes and the plugin reads every cycle in place of its ports.
//!
//! All are created only for HushMic's own chain (the app passes the paths
//! in `HUSHMIC_METER` and `HUSHMIC_CONTROL`); other hosts of the plugin
//! never get one. The writers never block, allocate or make a system call
//! per hop; readers never block the writer and detect torn reads instead.

pub mod control;
pub mod meter;
mod region;
pub mod tap;

pub use region::{monotonic_ms, test_dir_base, Region, PAGE};

/// Environment variable naming the meter file of HushMic's own chain.
pub const METER_ENV: &str = "HUSHMIC_METER";

/// Environment variable naming the control file of HushMic's own chain.
pub const CONTROL_ENV: &str = "HUSHMIC_CONTROL";
