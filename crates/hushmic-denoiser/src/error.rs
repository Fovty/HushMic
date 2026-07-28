use std::fmt;

/// Everything that can go wrong, split by what the embedding application can
/// do about it: point the user at an ONNX Runtime install (`Runtime`), fix the
/// model file or path (`Model`), or log-and-continue (`Inference`, which the
/// engine already degrades through gracefully).
// No PartialEq: error equality would compare message strings, inviting
// downstream tests to couple to exact wording; match on the kind instead.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Error {
    /// ONNX Runtime could not be located or loaded.
    Runtime(String),
    /// The model file/bytes could not be loaded into a session.
    Model(String),
    /// A single inference step failed; output was still produced (near-silence)
    /// and the stream stays aligned, so processing can continue.
    Inference(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Runtime(m) | Error::Model(m) | Error::Inference(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_the_message_and_kinds_are_matchable() {
        let e = Error::Runtime("no runtime".into());
        assert_eq!(e.to_string(), "no runtime");
        // non_exhaustive: downstream matches need a catch-all, ours don't
        match e {
            Error::Runtime(_) => {}
            _ => panic!("kind must round-trip"),
        }
    }
}
