use crate::denoiser::Denoiser;
use crate::error::Error;
use crate::stft::HOP;

/// Chunk-size-agnostic wrapper around a [`Denoiser`]: feed whatever your
/// input source hands you, get denoised samples back as whole hops complete.
///
/// Inference errors never interrupt the stream — the engine emits aligned
/// near-silence for the failed hop and recovers on the next good one, exactly
/// like the plugin build. Observe them via [`StreamDenoiser::take_error`].
///
/// Buffers grow to the largest chunk seen and are reused after that; at a
/// steady chunk size there is no per-call allocation.
pub struct StreamDenoiser {
    denoiser: Denoiser,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    fed: u64,
    last_error: Option<Error>,
}

impl StreamDenoiser {
    /// Wrap a configured [`Denoiser`]; configure mode/attenuation before or
    /// after via [`StreamDenoiser::denoiser_mut`].
    pub fn new(denoiser: Denoiser) -> StreamDenoiser {
        StreamDenoiser {
            denoiser,
            in_buf: Vec::with_capacity(2 * HOP),
            out_buf: Vec::with_capacity(2 * HOP),
            fed: 0,
            last_error: None,
        }
    }

    /// Feed any number of samples; returns the denoised samples that became
    /// available (empty until a full hop has accumulated). Output trails
    /// input by [`Denoiser::latency_samples`]. The returned slice borrows the
    /// internal buffer — copy it out before the next call.
    pub fn process(&mut self, input: &[f32]) -> &[f32] {
        self.fed += input.len() as u64;
        self.in_buf.extend_from_slice(input);
        self.out_buf.clear();
        let mut hop_in = [0f32; HOP];
        let mut hop_out = [0f32; HOP];
        let whole = self.in_buf.len() / HOP;
        for h in 0..whole {
            hop_in.copy_from_slice(&self.in_buf[h * HOP..(h + 1) * HOP]);
            if let Err(e) = self.denoiser.process_hop(&hop_in, &mut hop_out) {
                self.last_error = Some(e);
            }
            self.out_buf.extend_from_slice(&hop_out);
        }
        self.in_buf.drain(..whole * HOP);
        &self.out_buf
    }

    /// The most recent inference error since the last call, if any
    /// (take semantics: returns it once, then `None` until the next error).
    pub fn take_error(&mut self) -> Option<Error> {
        self.last_error.take()
    }

    /// Exactly how many zero samples to feed so that every real input
    /// sample's denoised counterpart has been emitted: the latency plus the
    /// fill to the next hop boundary. Lets file processors drain the tail:
    /// feed `pending()` zeros, then truncate the total output to the input
    /// length.
    pub fn pending(&self) -> usize {
        let fill = (HOP as u64 - self.fed % HOP as u64) % HOP as u64;
        self.denoiser.latency_samples() + fill as usize
    }

    /// The wrapped [`Denoiser`] (mode/attenuation getters live there).
    pub fn denoiser(&self) -> &Denoiser {
        &self.denoiser
    }

    /// Mutable access for [`Denoiser::set_mode`] /
    /// [`Denoiser::set_attenuation_limit_db`]; changes apply from the next
    /// full hop.
    pub fn denoiser_mut(&mut self) -> &mut Denoiser {
        &mut self.denoiser
    }

    /// Recover the inner [`Denoiser`], dropping any buffered samples.
    pub fn into_inner(self) -> Denoiser {
        self.denoiser
    }

    /// Clear buffered samples and reset the inner [`Denoiser`] (its mode and
    /// attenuation limit survive, per [`Denoiser::reset`]).
    pub fn reset(&mut self) {
        self.in_buf.clear();
        self.out_buf.clear();
        self.fed = 0;
        self.last_error = None;
        self.denoiser.reset();
    }
}

#[cfg(test)]
mod tests {
    // pending() is pure arithmetic over `fed`; the paths that need a model
    // live in tests/api.rs. Construct-free check via the formula itself:
    // (fed, expected fill): 0 -> 0, 1 -> 479, 480 -> 0, 1000 -> 440.
    use crate::stft::HOP;

    fn fill(fed: u64) -> u64 {
        (HOP as u64 - fed % HOP as u64) % HOP as u64
    }

    #[test]
    fn pending_fill_reaches_the_next_hop_boundary() {
        assert_eq!(fill(0), 0);
        assert_eq!(fill(1), 479);
        assert_eq!(fill(479), 1);
        assert_eq!(fill(480), 0);
        assert_eq!(fill(1000), 440);
        // draining property: fed + fill is always hop-aligned
        for fed in [0u64, 1, 479, 480, 481, 999, 1000, 12345] {
            assert_eq!((fed + fill(fed)) % HOP as u64, 0, "fed={fed}");
        }
    }
}
