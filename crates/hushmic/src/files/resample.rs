//! Sample-rate conversion to and from the engine's 48 kHz.
//!
//! The output is exactly as long as the input at the new rate (rounded to
//! the nearest frame) and rubato's own delay is taken out, so the converted
//! sound starts on the same instant as the original: the spike's converter
//! was 320 frames (6.7 ms) late without this.

use rubato::{FftFixedInOut, Resampler};

/// Input frames per conversion step (rubato rounds it to its FFT size).
const CHUNK: usize = 1024;

pub struct Converter {
    inner: Option<FftFixedInOut<f32>>,
    from: u32,
    to: u32,
    channels: usize,
    /// Input still waiting for a full chunk.
    pending: Vec<Vec<f32>>,
    /// rubato's delay, in output frames, still to be thrown away.
    to_drop: usize,
    taken: u64,
    given: u64,
}

/// Output frames for `frames` input frames: the same length in time,
/// rounded to the nearest frame.
pub fn converted_len(frames: u64, from: u32, to: u32) -> u64 {
    if from == to || from == 0 {
        return frames;
    }
    ((frames as u128 * to as u128 + from as u128 / 2) / from as u128) as u64
}

impl Converter {
    /// None when rubato refuses the rates (zero, or a ratio it cannot
    /// build); equal rates pass through untouched.
    pub fn new(from: u32, to: u32, channels: usize) -> Option<Converter> {
        let channels = channels.max(1);
        let inner = if from == to {
            None
        } else {
            Some(FftFixedInOut::<f32>::new(from as usize, to as usize, CHUNK, channels).ok()?)
        };
        let to_drop = inner.as_ref().map_or(0, |r| r.output_delay());
        Some(Converter {
            inner,
            from,
            to,
            channels,
            pending: vec![Vec::new(); channels],
            to_drop,
            taken: 0,
            given: 0,
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Convert a block (one Vec per channel, all the same length). Returns
    /// whatever is ready; the rest comes with later blocks or `finish`.
    pub fn push(&mut self, input: &[Vec<f32>]) -> Vec<Vec<f32>> {
        let n = input.first().map_or(0, Vec::len);
        self.taken += n as u64;
        let Some(r) = self.inner.as_mut() else {
            self.given += n as u64;
            return input.to_vec();
        };
        for (p, i) in self.pending.iter_mut().zip(input) {
            p.extend_from_slice(i);
        }
        let mut out = vec![Vec::new(); self.channels];
        loop {
            let need = r.input_frames_next();
            if self.pending[0].len() < need {
                break;
            }
            let chunk: Vec<&[f32]> = self.pending.iter().map(|p| &p[..need]).collect();
            if let Ok(o) = r.process(&chunk, None) {
                append(&mut out, o, &mut self.to_drop);
            }
            for p in &mut self.pending {
                p.drain(..need);
            }
        }
        self.given += out[0].len() as u64;
        out
    }

    /// The tail: what is still inside the converter, cut to the exact
    /// length for everything pushed.
    pub fn finish(&mut self) -> Vec<Vec<f32>> {
        let want = converted_len(self.taken, self.from, self.to);
        let mut out = vec![Vec::new(); self.channels];
        let Some(r) = self.inner.as_mut() else {
            return out;
        };
        let mut first = true;
        while self.given + (out[0].len() as u64) < want {
            let res = if first {
                let rest: Vec<&[f32]> = self.pending.iter().map(|p| p.as_slice()).collect();
                r.process_partial(Some(&rest), None)
            } else {
                r.process_partial::<&[f32]>(None, None)
            };
            first = false;
            match res {
                Ok(o) if !o.is_empty() && !o[0].is_empty() => {
                    append(&mut out, o, &mut self.to_drop)
                }
                _ => break,
            }
        }
        for p in &mut self.pending {
            p.clear();
        }
        let keep = want.saturating_sub(self.given) as usize;
        for c in &mut out {
            c.truncate(keep);
        }
        self.given += out[0].len() as u64;
        out
    }
}

/// Append converted frames, dropping the first `to_drop` (rubato's delay).
fn append(out: &mut [Vec<f32>], mut o: Vec<Vec<f32>>, to_drop: &mut usize) {
    let n = o.first().map_or(0, Vec::len);
    let d = (*to_drop).min(n);
    *to_drop -= d;
    for (dst, src) in out.iter_mut().zip(o.iter_mut()) {
        dst.extend_from_slice(&src[d..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Impulses at known input frames land on the matching output frame
    /// (within one) and the length is exact, for the rates people record at.
    #[test]
    fn impulses_keep_their_time_and_the_length_is_exact() {
        for &(from, to) in &[
            (44_100, 48_000),
            (48_000, 44_100),
            (8_000, 48_000),
            (22_050, 48_000),
            (96_000, 48_000),
            (32_000, 48_000),
        ] {
            let len = from as usize * 2 + 123;
            let at = [from as usize / 3, from as usize, len - from as usize / 4];
            let mut x = vec![0f32; len];
            for &a in &at {
                x[a] = 1.0;
            }
            let mut c = Converter::new(from, to, 2).unwrap();
            let mut y: Vec<f32> = Vec::new();
            // Uneven blocks, as decoders deliver them.
            for block in x.chunks(777) {
                let o = c.push(&[block.to_vec(), block.to_vec()]);
                assert_eq!(o[0], o[1]);
                y.extend_from_slice(&o[0]);
            }
            y.extend_from_slice(&c.finish()[0]);
            assert_eq!(
                y.len() as u64,
                converted_len(len as u64, from, to),
                "{from}->{to}"
            );
            for &a in &at {
                let want = (a as f64 * to as f64 / from as f64).round() as i64;
                let lo = (want - 40).max(0) as usize;
                let hi = ((want + 40) as usize).min(y.len());
                let (peak, _) = y[lo..hi].iter().enumerate().fold((0, 0f32), |b, (i, v)| {
                    if v.abs() > b.1 {
                        (i, v.abs())
                    } else {
                        b
                    }
                });
                let got = (lo + peak) as i64;
                assert!(
                    (got - want).abs() <= 1,
                    "{from}->{to}: impulse at {a} -> {got}, want {want}"
                );
            }
        }
    }

    #[test]
    fn equal_rates_pass_through() {
        let mut c = Converter::new(48_000, 48_000, 1).unwrap();
        let x = vec![vec![0.5f32; 100]];
        assert_eq!(c.push(&x), x);
        assert!(c.finish()[0].is_empty());
    }

    #[test]
    fn converted_length_rounds_to_the_nearest_frame() {
        assert_eq!(converted_len(44_100, 44_100, 48_000), 48_000);
        assert_eq!(converted_len(1, 44_100, 48_000), 1);
        assert_eq!(converted_len(441, 44_100, 48_000), 480);
        assert_eq!(converted_len(7, 48_000, 48_000), 7);
    }
}
