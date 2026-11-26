//! "Clean a file": the offline path from a recording to its cleaned copy.
//! Decoding with exact timing ([`decode`], [`mp4`]), the conversion to and
//! from the engine's 48 kHz ([`resample`]), the engine ([`engine`]), the
//! cleaned sound on disk ([`cache`]) with the file's picture
//! ([`picture`]), the pass that fills both ([`pass`]),
//! every strength as a mix of it and the original ([`mix`]), playing it
//! ([`player`]), saving it ([`save`], through [`ffmpeg`] where there is
//! one), and the process the daemon runs a pass in ([`worker`]), one per
//! file of the daemon's [`jobs`].

pub mod cache;
pub mod chooser;
pub mod cli;
pub mod decode;
pub mod docpath;
pub mod engine;
pub mod ffmpeg;
pub mod gather;
pub mod held;
pub mod jobs;
pub mod kinds;
pub mod mix;
pub mod mp4;
pub mod note;
pub mod pass;
pub mod picture;
pub mod player;
pub mod probe;
pub mod resample;
pub mod reveal;
pub mod save;
pub mod worker;
