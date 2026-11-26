//! Alignment for the fallback source (two recording streams instead of
//! the engine tap): how far the clean capture trails the raw one.
//!
//! The two `pw-record` streams start at different times and the engine
//! adds its own latency, so the offset is unknown but constant within a
//! take (both run on one PipeWire clock). Voice survives the engine at
//! about its raw level, so the two sides correlate where someone speaks
//! even though the noise does not:
//! 1. a coarse search on 10 ms level envelopes over the whole take;
//! 2. a fine search on the waveform, sample by sample, within two blocks
//!    of the coarse peak, over the loudest voiced two seconds, with every
//!    frequency weighted equally (GCC-PHAT) so the pitch period of a voice
//!    cannot pose as the lag;
//! 3. a check that both halves of the take agree (a stream that dropped
//!    data shifts the offset mid-take); when they do not, the newer half
//!    is used alone if its quarters agree, otherwise nothing.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

const BLOCK: usize = 480;
/// Coarse correlation needed to trust the envelope peak.
const MIN_COARSE: f32 = 0.4;
/// Prominence (peak over the RMS of the searched range) needed to trust
/// the sample-exact peak; below it the coarse estimate (10 ms) stands,
/// which the 20 ms crossfade hides.
const MIN_FINE: f32 = 8.0;
/// The fine search's reach around the coarse peak: two blocks, since the
/// envelope peak can sit one block off.
const REACH: usize = 2 * BLOCK;
/// The plain correlation refines the phase-only peak within this (1 ms,
/// shorter than any voice's pitch period).
const PRECISE: usize = 48;
/// The halves of a take may disagree by this much (samples).
const HALVES_TOLERANCE: i64 = 240;
/// The fine search's window.
const FINE_WINDOW: usize = 96_000;
/// A change of lag inside a take is placed to within this (100 ms).
const SETTLE_WINDOW: usize = 4_800;
/// A stretch pairs at the old lag when its correlation there is at
/// least this, and by `SETTLE_MARGIN` more than at the new lag (silence
/// pairs at neither).
const SETTLE_MIN: f32 = 0.3;
const SETTLE_MARGIN: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// `late[n]` is the sound of `early[n - lag]` (negative: `late` leads).
    pub lag: i64,
    /// The coarse envelope correlation at the peak, 0..1.
    pub confidence: f32,
    /// The fine search confirmed the lag to the sample.
    pub exact: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Alignment {
    Aligned(Estimate),
    /// The halves disagree (a stream dropped data) but the newer half
    /// holds together: the take from index `from` of both inputs on is
    /// aligned on its own. `from` is the middle, or later when the loss
    /// lies in the newer half: past the last 100 ms that still pairs at
    /// the old lag, with 100 ms to spare.
    AlignedTail {
        estimate: Estimate,
        from: usize,
    },
    /// No voice in common (a take of typing or fan noise only).
    NoPeak,
    /// The halves disagree and the second half does not hold together
    /// either: a stream dropped data late in the take.
    Inconsistent,
}

fn envelope(x: &[f32]) -> Vec<f32> {
    x.as_chunks::<BLOCK>()
        .0
        .iter()
        .map(|b| {
            let ms = b.iter().map(|&s| s * s).sum::<f32>() / BLOCK as f32;
            if ms > 1e-9 {
                10.0 * ms.log10()
            } else {
                -90.0
            }
        })
        .collect()
}

/// Pearson correlation of `a[i]` with `b[i + lag]` over their overlap.
fn corr_at(a: &[f32], b: &[f32], lag: i64) -> Option<f32> {
    let (a0, b0) = if lag >= 0 {
        (0usize, lag as usize)
    } else {
        ((-lag) as usize, 0usize)
    };
    let n = a.len().saturating_sub(a0).min(b.len().saturating_sub(b0));
    if n < 50 {
        return None;
    }
    let (a, b) = (&a[a0..a0 + n], &b[b0..b0 + n]);
    let ma = a.iter().sum::<f32>() / n as f32;
    let mb = b.iter().sum::<f32>() / n as f32;
    let (mut sab, mut saa, mut sbb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        let (dx, dy) = (x - ma, y - mb);
        sab += dx * dy;
        saa += dx * dx;
        sbb += dy * dy;
    }
    (saa > 0.0 && sbb > 0.0).then(|| sab / (saa * sbb).sqrt())
}

/// Coarse lag in blocks within `±max_blocks`, and its correlation.
fn coarse(early: &[f32], late: &[f32], max_blocks: i64) -> Option<(i64, f32)> {
    let (ea, la) = (envelope(early), envelope(late));
    let mut best: Option<(i64, f32)> = None;
    for lag in -max_blocks..=max_blocks {
        if let Some(c) = corr_at(&ea, &la, lag) {
            if best.is_none_or(|(_, b)| c > b) {
                best = Some((lag, c));
            }
        }
    }
    best.filter(|&(_, c)| c >= MIN_COARSE)
}

/// Sample lag within `around ± REACH` by GCC-PHAT (cross-correlation
/// with every frequency weighted equally) over the loudest `FINE_WINDOW`
/// of `late`, with the peak's prominence over the searched range. Plain
/// cross-correlation of voice peaks at every pitch period as well; the
/// phase transform keeps only the timing, so the true lag stands out.
fn fine(early: &[f32], late: &[f32], around: i64) -> Option<(i64, f32)> {
    let win = FINE_WINDOW.min(late.len() / 2);
    if win < 4 * BLOCK {
        return None;
    }
    let reach = REACH as i64;
    // The loudest window of `late` whose partner range (early indices
    // best_start - around - REACH .. + win + 2 REACH) lies inside `early`.
    let lo = (around + reach).max(0);
    let hi = (late.len() as i64 - win as i64).min(early.len() as i64 + around - win as i64 - reach);
    if hi <= lo {
        return None;
    }
    let (mut best_start, mut best_e) = (lo as usize, -1f32);
    let mut start = lo;
    while start <= hi {
        let s = start as usize;
        let e: f32 = late[s..s + win].iter().step_by(8).map(|x| x * x).sum();
        if e > best_e {
            best_e = e;
            best_start = s;
        }
        start += 10 * BLOCK as i64;
    }
    let seg_late = &late[best_start..best_start + win];
    // early[n - lag] for lag in around-REACH..=around+REACH.
    let e0 = best_start as i64 - around - reach;
    if e0 < 0 || (e0 as usize) + win + 2 * REACH > early.len() {
        return None;
    }
    let seg_early = &early[e0 as usize..e0 as usize + win + 2 * REACH];
    let n = (win + 2 * REACH + win).next_power_of_two();
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let padded = |x: &[f32]| -> Vec<Complex<f32>> {
        x.iter()
            .map(|&v| Complex::new(v, 0.0))
            .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
            .take(n)
            .collect()
    };
    let mut a = padded(seg_early);
    let mut b = padded(seg_late);
    fwd.process(&mut a);
    fwd.process(&mut b);
    // r[k] = sum_t early_seg[t + k] * late_seg[t]: once plain, once phase
    // only over the frequencies where the clean side carries sound (within
    // 60 dB of its strongest; the rest is removed noise of random phase).
    // The phase-only peak picks the right period; the plain one, searched
    // within a millisecond of it, puts it on the exact sample.
    let floor = b.iter().map(|c| c.norm()).fold(0f32, f32::max) * 1e-3;
    let mut phat = vec![Complex::new(0.0f32, 0.0); n];
    for ((x, y), p) in a.iter_mut().zip(&b).zip(phat.iter_mut()) {
        let c = *x * y.conj();
        let m = c.norm();
        if y.norm() > floor && m > 0.0 {
            *p = c / m;
        }
        *x = c;
    }
    inv.process(&mut a);
    inv.process(&mut phat);
    let r: Vec<f32> = phat.iter().take(2 * REACH + 1).map(|c| c.re).collect();
    let (kp, peak) = r
        .iter()
        .copied()
        .enumerate()
        .max_by(|x, y| x.1.total_cmp(&y.1))?;
    let rms = (r.iter().map(|v| v * v).sum::<f32>() / r.len() as f32).sqrt();
    let prominence = if rms > 0.0 { peak / rms } else { 0.0 };
    let near = kp.saturating_sub(PRECISE)..=(kp + PRECISE).min(2 * REACH);
    let (k, _) = near
        .map(|k| (k, a[k].re))
        .max_by(|x, y| x.1.total_cmp(&y.1))?;
    // early index e0 + k + t pairs with late index best_start + t.
    let lag = best_start as i64 - (e0 + k as i64);
    Some((lag, prominence))
}

/// Estimate the lag of `late` behind `early` within `±max_lag` samples.
pub fn estimate(early: &[f32], late: &[f32], max_lag: usize) -> Option<Estimate> {
    let max_blocks = (max_lag / BLOCK) as i64 + 1;
    let (blocks, confidence) = coarse(early, late, max_blocks)?;
    let around = blocks * BLOCK as i64;
    match fine(early, late, around) {
        Some((lag, r)) if r >= MIN_FINE => Some(Estimate {
            lag,
            confidence,
            exact: true,
        }),
        _ => Some(Estimate {
            lag: around,
            confidence,
            exact: false,
        }),
    }
}

/// Normalised correlation of `late[s..s + w]` with the `early` it pairs
/// with at `lag`.
fn ncc_at(early: &[f32], late: &[f32], s: usize, w: usize, lag: i64) -> Option<f32> {
    let e = usize::try_from(s as i64 - lag).ok()?;
    let x = late.get(s..s + w)?;
    let y = early.get(e..e + w)?;
    let (mut xy, mut xx, mut yy) = (0f64, 0f64, 0f64);
    for (&a, &b) in x.iter().zip(y) {
        xy += f64::from(a) * f64::from(b);
        xx += f64::from(a) * f64::from(a);
        yy += f64::from(b) * f64::from(b);
    }
    (xx > 0.0 && yy > 0.0).then(|| (xy / (xx * yy).sqrt()) as f32)
}

/// The `late` index from which, within `lo..hi`, `late` pairs at `after`
/// rather than `before`: one window past the last window that pairs at
/// `before` (the loss may sit early in the next one), `lo` when none does.
fn settled(early: &[f32], late: &[f32], lo: usize, hi: usize, before: i64, after: i64) -> usize {
    let mut at = lo;
    let mut s = lo;
    while s + SETTLE_WINDOW <= hi {
        let old = ncc_at(early, late, s, SETTLE_WINDOW, before);
        let new = ncc_at(early, late, s, SETTLE_WINDOW, after);
        if let (Some(o), Some(n)) = (old, new) {
            if o >= SETTLE_MIN && o > n + SETTLE_MARGIN {
                at = s + 2 * SETTLE_WINDOW;
            }
        }
        s += SETTLE_WINDOW;
    }
    at
}

/// Align a take: estimate on the whole and on each half, and accept only
/// when the halves agree. When they do not (a stream lost a buffer, under
/// load typically one 10 ms quantum), the newer half is tried on its own:
/// its quarters must agree, and the take starts where the old lag ends.
pub fn align(early: &[f32], late: &[f32], max_lag: usize) -> Alignment {
    let Some(whole) = estimate(early, late, max_lag) else {
        return Alignment::NoPeak;
    };
    let half = early.len().min(late.len()) / 2;
    let first = estimate(&early[..half], &late[..half], max_lag);
    let second = estimate(&early[half..], &late[half..], max_lag);
    let apart = |a: &Estimate, b: &Estimate| (a.lag - b.lag).abs() > HALVES_TOLERANCE;
    match (first, second) {
        (Some(a), Some(b)) if apart(&a, &b) => {
            let n = early.len().min(late.len());
            let quarter = half + (n - half) / 2;
            let third = estimate(&early[half..quarter], &late[half..quarter], max_lag);
            let fourth = estimate(&early[quarter..n], &late[quarter..n], max_lag);
            match (third, fourth) {
                (Some(c), Some(d)) if b.exact && !apart(&c, &d) && !apart(&b, &d) => {
                    // The loss lies before the third's loudest voice, so
                    // before `quarter`, maybe after the middle. Without the
                    // old lag to the sample, start at `quarter`.
                    let on_new = if a.exact {
                        settled(early, late, half, quarter, a.lag, b.lag)
                    } else {
                        quarter
                    };
                    let from = on_new.saturating_sub(b.lag.max(0) as usize).max(half);
                    Alignment::AlignedTail { estimate: b, from }
                }
                _ => Alignment::Inconsistent,
            }
        }
        // A half without speech says nothing against the whole.
        _ => Alignment::Aligned(whole),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
    }

    /// Voiced bursts (a few harmonics with a moving pitch, syllable
    /// envelopes and pauses) of `secs` at 48 kHz.
    fn voice(secs: f32, seed: u64) -> Vec<f32> {
        let n = (secs * 48_000.0) as usize;
        let mut rng = Lcg(seed);
        let mut syllables = Vec::new();
        let mut t = 0usize;
        while t < n {
            let len = 6_000 + (rng.next().abs() * 12_000.0) as usize;
            let gap = 3_000 + (rng.next().abs() * 20_000.0) as usize;
            syllables.push((t, len, 110.0 + 60.0 * rng.next().abs()));
            t += len + gap;
        }
        let mut x = vec![0f32; n];
        for (start, len, f0) in syllables {
            for k in 0..len.min(n.saturating_sub(start)) {
                let env = (std::f32::consts::PI * k as f32 / len as f32).sin();
                let tt = (start + k) as f32 / 48_000.0;
                let f = f0 * (1.0 + 0.1 * (tt * 3.0).sin());
                let mut s = 0.0;
                for h in 1..6 {
                    s += (2.0 * std::f32::consts::PI * f * h as f32 * tt).sin() / h as f32;
                }
                x[start + k] = 0.2 * env * s;
            }
        }
        x
    }

    fn noise(n: usize, amp: f32, seed: u64) -> Vec<f32> {
        let mut rng = Lcg(seed);
        (0..n).map(|_| amp * rng.next()).collect()
    }

    /// raw = voice + noise; clean = voice (noise removed), delayed by
    /// `lag` relative to raw, with `late_start` extra samples in front.
    fn pair(lag: i64, secs: f32) -> (Vec<f32>, Vec<f32>) {
        let v = voice(secs + 1.0, 5);
        let nz = noise(v.len(), 0.05, 9);
        let raw: Vec<f32> = v.iter().zip(&nz).map(|(a, b)| a + b).collect();
        let n = (secs * 48_000.0) as usize;
        let pad = 24_000usize;
        let raw_take = raw[pad..pad + n].to_vec();
        let clean_take: Vec<f32> = (0..n)
            .map(|i| {
                let j = pad as i64 + i as i64 - lag;
                v[j as usize] * 0.9 + noise(1, 0.002, i as u64)[0]
            })
            .collect();
        (raw_take, clean_take)
    }

    #[test]
    fn finds_the_lag_to_the_sample_both_ways() {
        for lag in [4_321i64, 2_400, 7, -1_234] {
            let (raw, clean) = pair(lag, 10.0);
            match align(&raw, &clean, 12_000) {
                Alignment::Aligned(e) => {
                    assert!(e.exact, "lag {lag}: {e:?}");
                    assert_eq!(e.lag, lag, "{e:?}");
                    assert!(e.confidence > 0.6, "{e:?}");
                }
                other => panic!("lag {lag}: {other:?}"),
            }
        }
    }

    #[test]
    fn noise_alone_has_no_peak() {
        let raw = noise(480_000, 0.1, 1);
        let clean = noise(480_000, 0.001, 2);
        assert_eq!(align(&raw, &clean, 12_000), Alignment::NoPeak);
    }

    #[test]
    fn a_dropout_in_the_first_half_keeps_the_second_aligned() {
        let (raw, clean) = pair(3_000, 10.0);
        // The clean stream lost one 10 ms quantum near the middle of the
        // take: from there it sits 480 samples earlier.
        let mut broken = clean[..230_000].to_vec();
        broken.extend_from_slice(&clean[230_480..]);
        broken.extend(std::iter::repeat_n(0.0, 480));
        match align(&raw, &broken, 12_000) {
            Alignment::AlignedTail { estimate, from } => {
                assert_eq!(from, 240_000);
                assert!(estimate.exact);
                assert_eq!(estimate.lag, 3_000 - 480);
            }
            other => panic!("{other:?}"),
        }
    }

    /// `x` without `len` samples at `at`, padded at the end.
    fn lose(x: &[f32], at: usize, len: usize) -> Vec<f32> {
        let mut y = x[..at].to_vec();
        y.extend_from_slice(&x[at + len..]);
        y.extend(std::iter::repeat_n(0.0, len));
        y
    }

    /// Whether `x[lo..hi]` holds voice (a 10 ms block above the pair's
    /// noise floor).
    fn voiced(x: &[f32], lo: usize, hi: usize) -> bool {
        lo < hi
            && x[lo..hi]
                .chunks(BLOCK)
                .any(|b| (b.iter().map(|v| v * v).sum::<f32>() / b.len() as f32).sqrt() > 0.02)
    }

    #[test]
    fn a_take_after_a_dropout_holds_no_voice_at_the_old_lag() {
        let (raw, clean) = pair(3_000, 10.0);
        let half = 240_000;
        let mut tails = 0;
        // Losses in the newer half, a quantum and five.
        for at in [250_000, 280_000] {
            for size in [480, 2_400] {
                // The clean stream lost `size` at `at`: from there it sits
                // `size` earlier. Before the loss it is the original.
                let broken = lose(&clean, at, size);
                if let Alignment::AlignedTail { estimate, from } = align(&raw, &broken, 12_000) {
                    tails += 1;
                    assert_eq!(estimate.lag, 3_000 - size as i64, "clean {at} {size}");
                    let start = from + estimate.lag as usize;
                    assert!(
                        !voiced(&clean, start, at),
                        "clean {at} {size}: voice from {start}"
                    );
                    assert!(
                        start <= at.max(half) + 3_000 + 3 * SETTLE_WINDOW,
                        "clean {at} {size}: {start}"
                    );
                }
                // The raw stream lost it: the clean side pairs `size` later
                // from clean index `at + 3_000` on.
                let broken = lose(&raw, at, size);
                if let Alignment::AlignedTail { estimate, from } = align(&broken, &clean, 12_000) {
                    tails += 1;
                    assert_eq!(estimate.lag, 3_000 + size as i64, "raw {at} {size}");
                    let start = from + estimate.lag as usize;
                    assert!(
                        !voiced(&clean, start, at + 3_000),
                        "raw {at} {size}: voice from {start}"
                    );
                    assert!(
                        start <= at.max(half) + 3_000 + size + 3 * SETTLE_WINDOW,
                        "raw {at} {size}: {start}"
                    );
                }
            }
        }
        assert!(tails >= 6, "{tails} takes kept their newer part");
    }

    #[test]
    fn a_dropout_late_in_the_take_is_inconsistent() {
        let (raw, clean) = pair(3_000, 10.0);
        // Lost 50 ms in the last quarter's neighbourhood: the newer half
        // does not hold together either.
        let mut broken = clean[..330_000].to_vec();
        broken.extend_from_slice(&clean[332_400..]);
        broken.extend(std::iter::repeat_n(0.0, 2_400));
        assert_eq!(align(&raw, &broken, 12_000), Alignment::Inconsistent);
    }
}
