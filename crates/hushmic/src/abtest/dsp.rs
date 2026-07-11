//! Spectrum + level analysis for the A/B window: streaming STFT folded to
//! the UI's 64 log-spaced dBFS bins, plus a block-RMS level meter. All
//! per-hop buffers (FFT planner, scratch, window, bin map) live in the
//! structs so steady-state `feed` calls do not allocate.

use crate::abtest::types::{BINS, DB_FLOOR, FFT_SIZE, FREQ_HI, FREQ_LO, HOP, SAMPLE_RATE};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

/// RMS block size: ~66 ms at 48 kHz, i.e. ~15 level readings/s.
pub const LEVEL_BLOCK: usize = 3200;
/// Exponential smoothing factor applied once per completed level block.
const LEVEL_SMOOTH: f32 = 0.35;

/// NOTE on the MAX fold into log bins: taking the maximum of the FFT bins
/// inside each output bin keeps tonal lines crisp but reads broadband noise
/// a few dB hotter toward high frequencies (wider bins pick larger maxima).
/// Deliberate display trade-off — the summary metrics use their own
/// analysis and are unaffected.
/// Streaming STFT → `BINS` log-spaced dBFS bins (`FFT_SIZE` window, `HOP`
/// hop, Hann). `feed` consumes arbitrary chunk sizes and returns one
/// bins-array per completed hop; bins[0] = FREQ_LO.
pub struct SpectrumAnalyzer {
    /// Pending input: a hop completes once `FFT_SIZE` samples are queued,
    /// after which the front `HOP` samples are consumed (windows overlap
    /// by `FFT_SIZE - HOP`).
    pending: Vec<f32>,
    /// Periodic Hann window, precomputed once.
    window: Vec<f32>,
    /// Coherent-gain normalization `2 / sum(window)`: a full-scale sine
    /// reads ~0 dBFS in its peak bin.
    norm: f32,
    fft: Arc<dyn Fft<f32>>,
    fft_buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Per-FFT-bin dB values for bins 0..=FFT_SIZE/2, reused every hop.
    db: Vec<f32>,
    /// Output bin i takes MAX over FFT bins [start, end). Ranges whose
    /// frequency span contains no FFT bin are pre-resolved to the single
    /// FFT bin nearest their center, so every range is non-empty.
    ranges: [(usize, usize); BINS],
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|n| 0.5 * (1.0 - (std::f32::consts::TAU * n as f32 / FFT_SIZE as f32).cos()))
            .collect();
        let norm = 2.0 / window.iter().sum::<f32>();
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Self {
            pending: Vec::with_capacity(FFT_SIZE + HOP),
            window,
            norm,
            fft_buf: vec![Complex::default(); FFT_SIZE],
            scratch,
            fft,
            db: vec![DB_FLOOR; FFT_SIZE / 2 + 1],
            ranges: Self::bin_ranges(),
        }
    }

    /// Precompute the FFT-bin → output-bin fold. Output bin i spans
    /// [FREQ_LO·r^i, FREQ_LO·r^(i+1)) with r = (FREQ_HI/FREQ_LO)^(1/BINS);
    /// ceil() edges tile the FFT bins so each belongs to exactly one
    /// output bin.
    fn bin_ranges() -> [(usize, usize); BINS] {
        let n_half = FFT_SIZE / 2;
        let bin_hz = SAMPLE_RATE as f32 / FFT_SIZE as f32;
        let log_span = (FREQ_HI / FREQ_LO).ln();
        let edge = |i: usize| FREQ_LO * (log_span * i as f32 / BINS as f32).exp();
        let mut ranges = [(0usize, 0usize); BINS];
        for (i, range) in ranges.iter_mut().enumerate() {
            let (lo, hi) = (edge(i), edge(i + 1));
            let start = (lo / bin_hz).ceil() as usize;
            let end = ((hi / bin_hz).ceil() as usize).min(n_half + 1);
            *range = if start < end {
                (start, end)
            } else {
                // No FFT bin falls inside this narrow low-frequency bin:
                // take the bin nearest the geometric center.
                let k = (((lo * hi).sqrt() / bin_hz).round() as usize).min(n_half);
                (k, k + 1)
            };
        }
        ranges
    }

    pub fn feed(&mut self, samples: &[f32]) -> Vec<[f32; BINS]> {
        self.pending.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.pending.len() >= FFT_SIZE {
            out.push(self.analyze_front());
            self.pending.drain(..HOP);
        }
        out
    }

    /// Window + FFT + fold the front FFT_SIZE pending samples.
    fn analyze_front(&mut self) -> [f32; BINS] {
        for (slot, (&s, &w)) in self
            .fft_buf
            .iter_mut()
            .zip(self.pending.iter().zip(&self.window))
        {
            *slot = Complex::new(s * w, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.fft_buf, &mut self.scratch);
        for (db, c) in self.db.iter_mut().zip(&self.fft_buf) {
            // log10(0) = −∞ clamps to the floor, so silence is exact.
            *db = (20.0 * (c.norm() * self.norm).log10()).max(DB_FLOOR);
        }
        let mut bins = [DB_FLOOR; BINS];
        for (bin, &(start, end)) in bins.iter_mut().zip(&self.ranges) {
            *bin = self.db[start..end].iter().copied().fold(DB_FLOOR, f32::max);
        }
        bins
    }
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Streaming RMS level: one smoothed dBFS reading per `LEVEL_BLOCK`
/// (~66 ms) of input. The smoother starts at DB_FLOOR (silence) so the
/// meter rises from the floor when signal appears.
pub struct LevelMeter {
    sum_sq: f64,
    count: usize,
    smoothed: f32,
}

impl LevelMeter {
    pub fn new() -> Self {
        Self {
            sum_sq: 0.0,
            count: 0,
            smoothed: DB_FLOOR,
        }
    }

    pub fn feed(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        for &s in samples {
            self.sum_sq += f64::from(s) * f64::from(s);
            self.count += 1;
            if self.count == LEVEL_BLOCK {
                let rms = (self.sum_sq / LEVEL_BLOCK as f64).sqrt() as f32;
                let db = (20.0 * rms.log10()).max(DB_FLOOR);
                self.smoothed += (db - self.smoothed) * LEVEL_SMOOTH;
                out.push(self.smoothed);
                self.sum_sq = 0.0;
                self.count = 0;
            }
        }
        out
    }
}

impl Default for LevelMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// Map dBFS to 0..=1 against the UI floor (DB_FLOOR → 0, 0 dBFS → 1).
pub fn db_to_unit(db: f32) -> f32 {
    ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| amp * (std::f32::consts::TAU * freq * n as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    /// Expected output bin for a frequency, derived from the log mapping
    /// independently of the analyzer's precomputed table.
    fn expected_bin(freq: f32) -> usize {
        (((freq / FREQ_LO).ln() / (FREQ_HI / FREQ_LO).ln()) * BINS as f32).floor() as usize
    }

    fn argmax(bins: &[f32; BINS]) -> usize {
        bins.iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0
    }

    #[test]
    fn full_scale_sine_peaks_in_expected_bin_near_zero_dbfs() {
        let mut an = SpectrumAnalyzer::new();
        let frames = an.feed(&sine(1000.0, 1.0, FFT_SIZE + 3 * HOP));
        assert_eq!(frames.len(), 4);
        let last = frames.last().unwrap();
        assert_eq!(argmax(last), expected_bin(1000.0));
        // Worst-case Hann scalloping is 1.42 dB, so a full-scale sine off
        // the FFT grid must still read within 1.5 dB of 0 dBFS.
        assert!(
            last[argmax(last)].abs() < 1.5,
            "peak {} dBFS not within 1.5 dB of 0",
            last[argmax(last)]
        );
    }

    #[test]
    fn on_grid_sine_reads_zero_dbfs_tightly() {
        // 1500 Hz = FFT bin 64 exactly (48000/2048 = 23.4375 Hz/bin): no
        // scalloping, so coherent-gain normalization must be near-exact.
        let mut an = SpectrumAnalyzer::new();
        let frames = an.feed(&sine(1500.0, 1.0, FFT_SIZE));
        let bins = frames.last().unwrap();
        assert_eq!(argmax(bins), expected_bin(1500.0));
        assert!(
            bins[argmax(bins)].abs() < 0.05,
            "on-grid peak {} dBFS deviates from 0",
            bins[argmax(bins)]
        );
    }

    #[test]
    fn silence_reads_floor_in_every_bin() {
        let mut an = SpectrumAnalyzer::new();
        let frames = an.feed(&vec![0.0; FFT_SIZE + HOP]);
        assert_eq!(frames.len(), 2);
        for frame in &frames {
            for &b in frame.iter() {
                assert_eq!(b, DB_FLOOR);
            }
        }
    }

    #[test]
    fn hop_cadence_matches_contract() {
        let mut an = SpectrumAnalyzer::new();
        // Priming needs FFT_SIZE; every further HOP samples completes one
        // frame.
        assert_eq!(an.feed(&vec![0.0; FFT_SIZE - 1]).len(), 0);
        assert_eq!(an.feed(&[0.0]).len(), 1);
        assert_eq!(an.feed(&vec![0.0; HOP - 1]).len(), 0);
        assert_eq!(an.feed(&[0.0]).len(), 1);
        assert_eq!(an.feed(&vec![0.0; 5 * HOP]).len(), 5);
    }

    #[test]
    fn every_output_bin_range_is_nonempty_and_ordered() {
        let ranges = SpectrumAnalyzer::bin_ranges();
        let n_half = FFT_SIZE / 2;
        let mut prev_start = 0usize;
        for &(start, end) in &ranges {
            assert!(start < end, "empty range ({start}, {end})");
            assert!(end <= n_half + 1);
            assert!(start >= prev_start, "bin starts must ascend");
            prev_start = start;
        }
        // bins[0] must be the lowest frequency: its FFT bin sits near
        // FREQ_LO, far below the top bin's.
        assert!(ranges[0].0 < ranges[BINS - 1].0);
    }

    #[test]
    fn spectrum_streaming_invariance_bit_exact() {
        let signal = sine(432.0, 0.8, FFT_SIZE + 4 * HOP);
        let mut whole = SpectrumAnalyzer::new();
        let big = whole.feed(&signal);
        let mut incremental = SpectrumAnalyzer::new();
        let mut small: Vec<[f32; BINS]> = Vec::new();
        for &s in &signal {
            small.extend(incremental.feed(&[s]));
        }
        assert_eq!(big.len(), small.len());
        for (a, b) in big.iter().flatten().zip(small.iter().flatten()) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn level_meter_converges_on_half_amplitude_sine() {
        // RMS of a 0.5-amplitude sine = 0.5/√2 → 20·log10 = −9.0309 dBFS.
        let mut meter = LevelMeter::new();
        let readings = meter.feed(&sine(440.0, 0.5, 30 * LEVEL_BLOCK));
        assert_eq!(readings.len(), 30);
        let last = *readings.last().unwrap();
        assert!(
            (last - -9.0309).abs() < 1.0,
            "converged to {last} dBFS, expected ≈ −9.03"
        );
        // Smoothing must approach the target monotonically from the floor.
        assert!(readings[0] < readings[29]);
    }

    #[test]
    fn level_meter_silence_stays_at_floor() {
        let mut meter = LevelMeter::new();
        let readings = meter.feed(&vec![0.0; 4 * LEVEL_BLOCK]);
        assert_eq!(readings.len(), 4);
        for &r in &readings {
            assert_eq!(r, DB_FLOOR);
        }
    }

    #[test]
    fn level_meter_streaming_invariance_bit_exact() {
        let signal = sine(250.0, 0.3, 5 * LEVEL_BLOCK);
        let mut whole = LevelMeter::new();
        let big = whole.feed(&signal);
        let mut incremental = LevelMeter::new();
        let mut small = Vec::new();
        for &s in &signal {
            small.extend(incremental.feed(&[s]));
        }
        assert_eq!(big.len(), small.len());
        for (a, b) in big.iter().zip(&small) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn db_to_unit_endpoints_and_clamping() {
        assert_eq!(db_to_unit(DB_FLOOR), 0.0);
        assert_eq!(db_to_unit(0.0), 1.0);
        assert_eq!(db_to_unit(DB_FLOOR - 40.0), 0.0);
        assert_eq!(db_to_unit(12.0), 1.0);
        assert_eq!(db_to_unit(DB_FLOOR / 2.0), 0.5);
    }
}
