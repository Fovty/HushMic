//! Real-time DPDFNet noise suppression for voice, as used by
//! [HushMic](https://github.com/Fovty/HushMic): 48 kHz mono frames in,
//! cleaned frames out. CPU-only, no network access, `Send` types for the
//! construct-then-move-into-the-audio-thread pattern.
//!
//! ```no_run
//! use hushmic_denoiser::{Denoiser, StreamDenoiser};
//!
//! let denoiser = Denoiser::from_file("dpdfnet8_48khz_hr.onnx")?;
//! let mut stream = StreamDenoiser::new(denoiser);
//! # let mic_chunk: Vec<f32> = Vec::new();
//! // in your capture loop, any chunk size:
//! let cleaned: &[f32] = stream.process(&mic_chunk);
//! # Ok::<(), hushmic_denoiser::Error>(())
//! ```
//!
//! Two external pieces are needed at runtime, neither of which this crate
//! downloads for you (it makes no network calls, ever):
//! - a DPDFNet model file — see the crate README for where to get one;
//! - the ONNX Runtime shared library (default `load-dynamic` feature) —
//!   a distro package works out of the box; bundling apps call
//!   [`init_runtime`] first. Build with `default-features = false` to pick
//!   your own `ort` linking strategy instead.

mod attn;
mod denoiser;
mod error;
mod mode;
mod model;
mod runtime;
mod stft;
mod stream;

pub use denoiser::Denoiser;
pub use error::Error;
pub use mode::Mode;
#[cfg(feature = "load-dynamic")]
pub use runtime::{init_runtime, RuntimeInit};
pub use stream::StreamDenoiser;

/// The only sample rate the DPDFNet models run at.
pub const SAMPLE_RATE: u32 = 48_000;

/// Samples per processing hop (10 ms): [`Denoiser::process_hop`] consumes and
/// produces exactly this many.
pub const HOP: usize = stft::HOP;

/// Algorithmic latency of the shipped DPDFNet models in samples (50 ms):
/// one hop of STFT framing plus the models' four-hop group delay. Pinned by
/// measurement in this crate's latency tests; prefer
/// [`Denoiser::latency_samples`] in code that should survive future models.
pub const LATENCY_SAMPLES: usize = 5 * stft::HOP;

#[cfg(test)]
mod tests {
    /// Compile-time guarantee: the construct-then-move-into-the-audio-thread
    /// pattern is the primary consumer story and must not regress silently
    /// (e.g. by a future `Rc` field).
    #[test]
    fn denoiser_types_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<crate::Denoiser>();
        assert_send::<crate::StreamDenoiser>();
    }
}
