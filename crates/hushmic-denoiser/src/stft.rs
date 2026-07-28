use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;

pub const N_FFT: usize = 960;
pub const HOP: usize = 480;
pub const FREQ_BINS: usize = 481; // N_FFT/2 + 1
pub const SPEC_LEN: usize = FREQ_BINS * 2; // 962, interleaved re/im

/// Vorbis (sin-of-sin^2) window, COLA at 50% overlap.
pub fn vorbis_window() -> [f32; N_FFT] {
    let h = (N_FFT as f32) / 2.0;
    let mut w = [0f32; N_FFT];
    for (n, wn) in w.iter_mut().enumerate() {
        let s = (0.5 * std::f32::consts::PI * (n as f32 + 0.5) / h).sin();
        *wn = (0.5 * std::f32::consts::PI * s * s).sin();
    }
    w
}

/// Causal analysis STFT (center = false). Keeps a 960-sample ring; each hop shifts
/// in 480 new samples, windows the full 960, and emits one interleaved re/im frame.
pub struct Analysis {
    window: [f32; N_FFT],
    ring: [f32; N_FFT],
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
}

impl Analysis {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        Self {
            window: vorbis_window(),
            ring: [0f32; N_FFT],
            fft: planner.plan_fft_forward(N_FFT),
            scratch: vec![Complex32::new(0.0, 0.0); N_FFT],
        }
    }

    pub fn reset(&mut self) {
        self.ring = [0f32; N_FFT];
    }

    pub fn push_hop(&mut self, in_hop: &[f32], out_spec: &mut [f32; SPEC_LEN]) {
        debug_assert_eq!(in_hop.len(), HOP);
        // shift left by HOP, append new HOP samples
        self.ring.copy_within(HOP.., 0);
        self.ring[N_FFT - HOP..].copy_from_slice(in_hop);
        // windowed full-resolution FFT
        for i in 0..N_FFT {
            self.scratch[i] = Complex32::new(self.ring[i] * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);
        // take the first FREQ_BINS bins, interleave re/im
        for k in 0..FREQ_BINS {
            out_spec[2 * k] = self.scratch[k].re;
            out_spec[2 * k + 1] = self.scratch[k].im;
        }
    }
}

/// ISTFT + overlap-add. Mirrors the analysis window; emits one hop per frame.
pub struct Synthesis {
    window: [f32; N_FFT],
    ola: [f32; N_FFT],
    ifft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
}

impl Synthesis {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        Self {
            window: vorbis_window(),
            ola: [0f32; N_FFT],
            ifft: planner.plan_fft_inverse(N_FFT),
            scratch: vec![Complex32::new(0.0, 0.0); N_FFT],
        }
    }

    pub fn reset(&mut self) {
        self.ola = [0f32; N_FFT];
    }

    pub fn add_frame(&mut self, spec: &[f32; SPEC_LEN], out_hop: &mut [f32; HOP]) {
        // rebuild full hermitian spectrum from FREQ_BINS interleaved bins
        for k in 0..FREQ_BINS {
            self.scratch[k] = Complex32::new(spec[2 * k], spec[2 * k + 1]);
        }
        // Hermitian mirror: for a length-N_FFT real signal, X[j] = conj(X[N_FFT-j]).
        // DC (bin 0) and Nyquist (bin N_FFT/2 = 480) are real and already set above;
        // fill bins 481..=959 from their conjugate partners 479..=1.
        for j in FREQ_BINS..N_FFT {
            self.scratch[j] = self.scratch[N_FFT - j].conj();
        }
        self.ifft.process(&mut self.scratch);
        // rustfft inverse is unnormalized: divide by N_FFT; window; overlap-add
        let norm = 1.0 / (N_FFT as f32);
        self.ola.copy_within(HOP.., 0);
        for s in &mut self.ola[N_FFT - HOP..] {
            *s = 0.0;
        }
        for i in 0..N_FFT {
            self.ola[i] += self.scratch[i].re * norm * self.window[i];
        }
        out_hop.copy_from_slice(&self.ola[..HOP]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Feeding identity frames (analysis -> synthesis with no spectral change) must
    // reconstruct the input, delayed by one hop, because the vorbis window is COLA.
    #[test]
    fn analysis_then_synthesis_reconstructs_input() {
        let mut ana = Analysis::new();
        let mut syn = Synthesis::new();
        // deterministic pseudo-signal
        let total = HOP * 40;
        let input: Vec<f32> = (0..total)
            .map(|n| (n as f32 * 0.013).sin() * 0.5 + (n as f32 * 0.0007).sin() * 0.3)
            .collect();

        let mut out = vec![0f32; total];
        let mut spec = [0f32; SPEC_LEN];
        let mut hop_out = [0f32; HOP];
        let hops = total / HOP;
        for h in 0..hops {
            ana.push_hop(&input[h * HOP..(h + 1) * HOP], &mut spec);
            syn.add_frame(&spec, &mut hop_out); // pass-through spectrum
            out[h * HOP..(h + 1) * HOP].copy_from_slice(&hop_out);
        }

        // Compare with a one-hop (480-sample) delay; skip the first 2 hops (warm-up).
        let delay = HOP;
        let mut max_err = 0f32;
        for n in (2 * HOP)..(total - delay) {
            max_err = max_err.max((out[n + delay] - input[n]).abs());
        }
        assert!(max_err < 1e-3, "reconstruction error too large: {max_err}");
    }

    /// The STFT round trip alone delays by exactly one hop and reconstructs
    /// an impulse bit-transparently (COLA) — the framing half of the crate's
    /// declared latency.
    #[test]
    fn stft_roundtrip_delay_is_one_hop() {
        let mut a = Analysis::new();
        let mut s = Synthesis::new();
        let total = 40 * HOP;
        let impulse_at = 20 * HOP + 123; // deliberately mid-hop
        let mut out = Vec::with_capacity(total);
        let mut spec = [0f32; SPEC_LEN];
        for h in 0..(total / HOP) {
            let mut hop_in = [0f32; HOP];
            for (j, v) in hop_in.iter_mut().enumerate() {
                if h * HOP + j == impulse_at {
                    *v = 1.0;
                }
            }
            let mut hop_out = [0f32; HOP];
            a.push_hop(&hop_in, &mut spec);
            s.add_frame(&spec, &mut hop_out);
            out.extend_from_slice(&hop_out);
        }
        let mut peak = (0usize, 0f32);
        for (i, &v) in out.iter().enumerate() {
            if v.abs() > peak.1 {
                peak = (i, v.abs());
            }
        }
        assert_eq!(
            peak.0 - impulse_at,
            HOP,
            "STFT round trip must delay one hop"
        );
        assert!(
            (peak.1 - 1.0).abs() < 1e-4,
            "COLA reconstruction must be transparent, got peak amp {}",
            peak.1
        );
    }
}
