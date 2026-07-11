//! Shared contract of the live A/B test window: the frame stream the
//! backend produces and the commands the UI issues. Derives serde so a
//! later DBus/CLI split can reuse the exact same types over the wire.

use serde::{Deserialize, Serialize};

pub const BINS: usize = 64;
pub const SAMPLE_RATE: u32 = 48_000;
pub const RECORD_SECS: f32 = 10.0;
/// Spectrum frame cadence (per channel).
pub const SPECTRUM_HZ: f32 = 30.0;
/// FFT window / hop that realize ~SPECTRUM_HZ at 48 kHz.
pub const FFT_SIZE: usize = 2048;
pub const HOP: usize = 1600;
/// Log-spaced analysis band (the axis labels top out at 8 kHz; the top edge
/// leaves headroom so 8k sits inside the panel).
pub const FREQ_LO: f32 = 120.0;
pub const FREQ_HI: f32 = 10_000.0;
/// Meter/readout floor: below this the UI shows "−∞ dB".
pub const DB_FLOOR: f32 = -80.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Channel {
    Raw,
    Filtered,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Channel::Raw => "raw",
            Channel::Filtered => "filtered",
        }
    }
}

/// Two honest, alignment-free summary indicators for a recorded sample.
///
/// A live A/B capture is TWO independent `pw-record` streams — measured
/// ~100 ms out of lag with near-zero frame-to-frame correlation, so any
/// per-frame raw-vs-filtered comparison is meaningless. These indicators
/// compare the two recordings' *distributions* (own noise floor, own
/// loudest-speech level), which a time offset cannot corrupt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SampleMetrics {
    /// The take had enough speech dynamics to report voice retention. When
    /// false the voice card shows a "record clear speech" hint, never a
    /// fabricated 0 dB.
    pub voice_measurable: bool,
    /// Background-floor drop: raw floor minus the filtered floor, the latter
    /// clamped to a realistic −75 dBFS measurement floor so the model's
    /// near-silent gating cannot print sub-silence reductions. ≥ 0.
    pub background_reduction_db: f32,
    /// Raw noise floor (low-percentile full-band dBFS of the raw take).
    pub raw_floor_dbfs: f32,
    /// Filtered noise floor, clamped to the −75 dBFS display floor.
    pub filtered_floor_dbfs: f32,
    /// Typical voice-band speech level, filtered minus raw (≈ 0 = voice
    /// kept; strongly negative = voice ducked). Median of each stream's
    /// loudest speech-band frames — distributional, no alignment needed.
    pub voice_retention_db: f32,
    /// Raw typical speech level (voice-band dBFS).
    pub raw_speech_dbfs: f32,
    /// Filtered typical speech level (voice-band dBFS).
    pub filtered_speech_dbfs: f32,
}

/// Backend → UI stream. Everything the window renders arrives as one of
/// these; the UI holds no audio state of its own.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Frame {
    /// ~30 Hz per channel; `bins[0]` = lowest frequency, dBFS values.
    Spectrum {
        ch: Channel,
        bins: Vec<f32>,
    },
    /// ~15 Hz smoothed input levels.
    Level {
        raw_db: f32,
        filtered_db: f32,
    },
    /// Emitted on change; `ok=false` raises the error overlay.
    Device {
        ok: bool,
        name: String,
    },
    /// The backend armed a take (restarting the monitors first if needed).
    /// Authoritative: re-syncs a UI whose optimistic Recording was knocked
    /// out by a stale `MonitorStopped` racing the Record click.
    RecordStarted,
    /// A cancel reached the backend while the take was still armed: the
    /// buffers were dropped, the monitors keep running. Re-syncs a UI whose
    /// optimistic cancel raced its own `RecordStarted`.
    RecordCancelled,
    RecordProgress {
        secs_done: f32,
    },
    /// The 10 s sample is on disk (in the runtime dir) and playable. Always
    /// authoritative: the backend only emits it for a take it stored — a
    /// cancel that raced the completion still yields this playable sample.
    RecordDone,
    PlaybackProgress {
        secs: f32,
    },
    PlaybackDone,
    /// Live monitoring ended for a reason the UI did not initiate (capture
    /// stream died, sample store failed): drop to the sample view. Device
    /// loss has its own frame; this one never raises the error overlay.
    MonitorStopped,
    Metrics(SampleMetrics),
    /// Non-fatal backend failure surfaced as a toast (e.g. playback failed
    /// to start).
    Warn(String),
}

/// UI → backend commands (the record length is backend policy, not UI
/// input).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Command {
    StartMonitor,
    /// Contextual stop: playback → sample view; an armed recording →
    /// cancelled take (monitors keep running); else stop the live monitor.
    Stop,
    Record,
    Play(Channel),
    /// Error-overlay "Retry detection".
    RetryDevice,
}
