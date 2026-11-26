//! Ten seconds of made-up sound for mock windows, screenshots and tests:
//! someone talks at a desk with a hissing room around them and types
//! between the sentences. The original is the voice with the hiss and
//! the key clicks; the cleaned side is the voice alone, as HushMic at
//! full strength leaves it.
//!
//! Everything comes from one fixed seed, so the picture of it is the same
//! on every run. It loops: it begins and ends in a pause.

use super::spectrum::{self, Analyzer, Frozen, Look, Picture, COLS, COLUMN_BYTES};
use super::take::FrozenTake;
use super::types::{Engine, SAMPLE_RATE, TAKE_FRAMES};
use std::collections::VecDeque;
use std::f32::consts::TAU;
use std::sync::{Arc, OnceLock};

const RATE: f32 = SAMPLE_RATE as f32;
/// The voice's gains are worked out this often and run straight between.
const BLOCK: usize = 240;
/// The room: white noise, about -52 dBFS.
const HISS: f32 = 0.0044;

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    /// 0..1.
    fn unit(&mut self) -> f32 {
        (self.next() >> 8) as f32 / (1u32 << 24) as f32
    }

    fn between(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// -1..1.
    fn noise(&mut self) -> f32 {
        2.0 * self.unit() - 1.0
    }
}

/// A band-pass (RBJ, constant peak gain).
struct BandPass {
    b0: f32,
    a1: f32,
    a2: f32,
    x: [f32; 2],
    y: [f32; 2],
}

impl BandPass {
    fn new(hz: f32, q: f32) -> BandPass {
        let w = TAU * hz / RATE;
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        BandPass {
            b0: alpha / a0,
            a1: -2.0 * w.cos() / a0,
            a2: (1.0 - alpha) / a0,
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * (x - self.x[1]) - self.a1 * self.y[0] - self.a2 * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// The first four formants of a few vowels (Hz).
const VOWELS: [[f32; 4]; 6] = [
    [730.0, 1090.0, 2440.0, 3400.0],
    [270.0, 2290.0, 3010.0, 3600.0],
    [300.0, 870.0, 2240.0, 3300.0],
    [530.0, 1840.0, 2480.0, 3500.0],
    [570.0, 840.0, 2410.0, 3300.0],
    [660.0, 1720.0, 2410.0, 3450.0],
];
const FORMANT_WIDTH: [f32; 4] = [90.0, 130.0, 190.0, 260.0];
const FORMANT_GAIN: [f32; 4] = [1.0, 0.5, 0.22, 0.09];
/// Harmonics are made up to here; they fade out long before.
const VOICE_TOP_HZ: f32 = 7600.0;

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// One spoken word into `out` from frame `at`: a few syllables in one
/// breath. `syllables` are each one's length in frames, vowel and
/// loudness; the voice runs through them, dipping between two, its
/// formants moving from vowel to vowel, its pitch falling from `pitch`.
fn word(out: &mut [f32], rng: &mut Rng, at: usize, syllables: &[(usize, usize, f32)], pitch: f32) {
    let len: usize = syllables.iter().map(|s| s.0).sum();
    // each syllable's middle, as a share of the word
    let mut middles = Vec::with_capacity(syllables.len());
    let mut from = 0;
    for s in syllables {
        middles.push((from + s.0 / 2) as f32 / len as f32);
        from += s.0;
    }
    let wobble = (rng.between(0.0, TAU), rng.between(4.5, 6.5));
    let fall = rng.between(0.1, 0.22);
    let pitch_at = |t: f32| {
        pitch
            * (1.0 - fall * t)
            * (1.0 + 0.025 * (wobble.0 + wobble.1 * t * len as f32 / RATE).sin())
    };
    // where the voice is between two syllables' vowels, and how loud
    let place = |t: f32| -> (usize, usize, f32) {
        let next = middles.iter().position(|m| *m > t).unwrap_or(middles.len());
        if next == 0 {
            return (0, 0, 0.0);
        }
        if next == middles.len() {
            return (next - 1, next - 1, 0.0);
        }
        let (a, b) = (middles[next - 1], middles[next]);
        (next - 1, next, smooth((t - a) / (b - a)))
    };
    let harmonics = (VOICE_TOP_HZ / (pitch * (1.0 - fall) * 0.97)) as usize;
    let gains = |t: f32| -> Vec<f32> {
        let f0 = pitch_at(t);
        let (a, b, k) = place(t);
        (1..=harmonics)
            .map(|h| {
                let hz = h as f32 * f0;
                let mut g = 0.03 * (-hz / 2200.0).exp();
                for j in 0..4 {
                    let (fa, fb) = (VOWELS[syllables[a].1][j], VOWELS[syllables[b].1][j]);
                    let d = (hz - (fa + (fb - fa) * k)) / FORMANT_WIDTH[j];
                    g += FORMANT_GAIN[j] / (1.0 + d * d);
                }
                g / (h as f32).powf(0.55)
            })
            .collect()
    };
    let loud = |t: f32| -> f32 {
        let (a, b, k) = place(t);
        let level = syllables[a].2 + (syllables[b].2 - syllables[a].2) * k;
        // a dip between two syllables
        level * (1.0 - 0.62 * (std::f32::consts::PI * k).sin().powi(2))
    };
    let (attack, release) = (0.035 * RATE, 0.09 * RATE);
    let mut breath = BandPass::new(3400.0, 0.9);
    let mut phase = 0.0f32;
    let mut g0 = gains(0.0);
    let mut n = 0;
    while n < len {
        let block = BLOCK.min(len - n);
        let g1 = gains((n + block) as f32 / len as f32);
        for i in 0..block {
            let m = n + i;
            let t = m as f32 / len as f32;
            phase += TAU * pitch_at(t) / RATE;
            if phase > TAU {
                phase -= TAU;
            }
            // sin(h·φ) for every harmonic, from the first two
            let (s1, c1) = phase.sin_cos();
            let (mut prev, mut cur) = (0.0f32, s1);
            let k = i as f32 / block as f32;
            let mut sum = 0.0;
            for h in 0..harmonics {
                sum += (g0[h] + (g1[h] - g0[h]) * k) * cur;
                let next = 2.0 * c1 * cur - prev;
                prev = cur;
                cur = next;
            }
            let env = smooth(m as f32 / attack) * smooth((len - m) as f32 / release) * loud(t);
            if let Some(o) = out.get_mut(at + m) {
                *o += env * (sum + 0.02 * breath.run(rng.noise()));
            }
        }
        g0 = g1;
        n += block;
    }
}

/// A consonant's hiss ("s", "sh", "f") into `out`: a patch of noise high
/// up.
fn fricative(out: &mut [f32], rng: &mut Rng, at: usize, len: usize, hz: f32, amp: f32) {
    let mut bands = [BandPass::new(hz, 2.2), BandPass::new(hz * 1.25, 1.6)];
    for m in 0..len {
        let t = m as f32 / len as f32;
        let env = (std::f32::consts::PI * t).sin().powf(1.2);
        let x = bands[0].run(rng.noise());
        let y = bands[1].run(x) * 2.0;
        if let Some(o) = out.get_mut(at + m) {
            *o += amp * env * y;
        }
    }
}

/// A key going down, and coming up again softer a moment later.
fn key_click(out: &mut [f32], rng: &mut Rng, at: usize) {
    let up = at + (rng.between(0.055, 0.09) * RATE) as usize;
    for (start, amp) in [(at, 1.0), (up, 0.35)] {
        let mut band = BandPass::new(rng.between(2400.0, 4200.0), 0.6);
        let thump = rng.between(140.0, 190.0);
        for m in 0..(0.03 * RATE) as usize {
            let t = m as f32 / RATE;
            let click = (-t / 0.0028).exp() * (band.run(rng.noise()) * 0.9 + rng.noise() * 0.12);
            let body = (-t / 0.009).exp() * (TAU * thump * t).sin() * 0.25;
            if let Some(o) = out.get_mut(start + m) {
                *o += amp * 0.34 * (click + body);
            }
        }
    }
}

/// The ten seconds: (the original, the cleaned side), frame for frame.
pub fn audio() -> (Vec<f32>, Vec<f32>) {
    let mut rng = Rng(0x48_75_73_68);
    let mut clean = vec![0.0f32; TAKE_FRAMES];
    let frames = |secs: f32| (secs * RATE) as usize;
    // sentences of a few words, with pauses to type in
    let mut pauses = vec![(0.0f32, 0.0f32)];
    let mut t = 0.5f32;
    while t < 8.4 {
        let words = 2 + (rng.next() % 3) as usize;
        let mut pitch = rng.between(118.0, 150.0);
        for _ in 0..words {
            if rng.unit() < 0.45 {
                let len = rng.between(0.07, 0.12);
                let hz = rng.between(4200.0, 6400.0);
                fricative(&mut clean, &mut rng, frames(t), frames(len), hz, 0.05);
                t += len * 0.8;
            }
            let count = 1 + (rng.next() % 3) as usize;
            let mut vowel = (rng.next() % 6) as usize;
            let syllables: Vec<(usize, usize, f32)> = (0..count)
                .map(|_| {
                    vowel = (vowel + 1 + (rng.next() % 5) as usize) % 6;
                    (
                        frames(rng.between(0.12, 0.22)),
                        vowel,
                        rng.between(0.07, 0.13),
                    )
                })
                .collect();
            let len: usize = syllables.iter().map(|s| s.0).sum();
            word(&mut clean, &mut rng, frames(t), &syllables, pitch);
            pitch *= rng.between(0.9, 1.02);
            t += len as f32 / RATE + rng.between(0.03, 0.1);
        }
        let pause = rng.between(0.6, 1.05);
        pauses.push((t, t + pause));
        t += pause;
    }
    pauses[0].1 = 0.5;
    let mut raw = clean.clone();
    // typing: in the pauses, and running on into the next words
    for &(from, to) in &pauses[1..] {
        let mut t = from + rng.between(0.1, 0.2);
        while t < (to + 0.25).min(9.8) {
            key_click(&mut raw, &mut rng, frames(t));
            t += rng.between(0.17, 0.36);
        }
    }
    // the room
    let mut low = 0.0f32;
    for r in raw.iter_mut() {
        let white = rng.noise();
        low += 0.11 * (white - low);
        *r += HISS * (white + 2.4 * low);
    }
    (raw, clean)
}

/// The ten seconds as a take recorded at full strength.
pub fn take() -> FrozenTake {
    let (raw, clean) = audio();
    let mut take = FrozenTake::from_audio(raw, clean);
    take.modes = vec![Engine::Filtering; take.columns.len()];
    take.strengths = vec![100.0; take.columns.len()];
    take
}

/// The spectrum of [`take`], worked out once.
pub fn spectrum() -> &'static Frozen {
    static SPECTRUM: OnceLock<Frozen> = OnceLock::new();
    SPECTRUM.get_or_init(|| Frozen::of(&mut Analyzer::new(), &take()))
}

/// A mock source's picture: the ten seconds on a loop, column by
/// column, each as it would have been heard in the mode and at the
/// strength of its moment.
pub struct Lanes {
    picture: Arc<Picture>,
    /// The next column of the loop.
    at: usize,
    /// What is in the live lanes: each column's place in the loop and its
    /// mode.
    live: VecDeque<(usize, Engine, f32)>,
    /// The same for the frozen take.
    frozen: Vec<(usize, Engine)>,
}

impl Default for Lanes {
    fn default() -> Self {
        Self::new()
    }
}

impl Lanes {
    pub fn new() -> Lanes {
        Lanes {
            picture: Arc::new(Picture::new()),
            at: 0,
            live: VecDeque::with_capacity(COLS + 1),
            frozen: Vec::new(),
        }
    }

    pub fn picture(&self) -> Arc<Picture> {
        Arc::clone(&self.picture)
    }

    fn column(at: usize, mode: Engine, db: f32) -> ([u8; COLUMN_BYTES], Look) {
        let mut column = [0u8; COLUMN_BYTES];
        let cells = spectrum().cells(at % COLS);
        let look = spectrum::levels(cells, spectrum::k_from_full(db), mode, &mut column);
        (column, look)
    }

    /// One more timeline column (50 ms), heard in `mode` at `db`.
    pub fn step(&mut self, mode: Engine, db: f32) {
        for _ in 0..spectrum::HOPS_PER_COLUMN {
            let (column, look) = Self::column(self.at, mode, db);
            self.picture.push(&column, look);
            self.live.push_back((self.at, mode, db));
            if self.live.len() > COLS {
                self.live.pop_front();
            }
            self.at += 1;
        }
    }

    /// The live lanes start over.
    pub fn clear(&mut self) {
        self.live.clear();
        self.picture.clear();
    }

    /// Freeze what the live lanes show, as a take cleaned at `db`.
    pub fn freeze(&mut self, db: f32) {
        self.frozen = self.live.iter().map(|&(at, mode, _)| (at, mode)).collect();
        self.restrength(db);
    }

    /// The frozen take at another strength.
    pub fn restrength(&mut self, db: f32) {
        if !self.frozen.is_empty() {
            self.picture.freeze(
                self.frozen
                    .iter()
                    .map(|&(at, mode)| Self::column(at, mode, db)),
            );
        }
    }

    pub fn show_frozen(&self, on: bool) {
        self.picture.show_frozen(on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::miccheck::spectrum::{row_of, Cursor, ROWS};

    #[test]
    fn the_made_up_take_is_the_same_every_time_and_cleans_to_the_voice() {
        let (raw, clean) = audio();
        assert_eq!((raw.len(), clean.len()), (TAKE_FRAMES, TAKE_FRAMES));
        assert_eq!(audio().0[..48_000], raw[..48_000]);
        assert!(raw.iter().chain(&clean).all(|x| x.abs() < 1.0));
        let s = spectrum();
        assert_eq!(s.cols(), COLS);
        let mut col = [0u8; COLUMN_BYTES];
        // it opens in a pause: the room on top, nothing below
        s.column(6, &mut col);
        assert!(col[row_of(3000.0).unwrap()] > 0);
        assert!(col[ROWS..].iter().all(|&l| l == 0));
        // and somewhere a voice is in both lanes
        let voiced = (0..COLS)
            .filter(|j| {
                s.column(*j, &mut col);
                col[ROWS..].iter().filter(|&&l| l > 100).count() >= 4
            })
            .count();
        assert!((COLS / 4..COLS * 3 / 4).contains(&voiced), "{voiced}");
    }

    /// A mock source's lanes follow the mode and the strength, live and
    /// frozen.
    #[test]
    fn a_mock_sources_lanes_follow_mode_and_strength() {
        let mut lanes = Lanes::new();
        let picture = lanes.picture();
        // the clean lane's sum of levels in the columns read
        let read = |cursor: &mut Cursor| {
            let (mut clean, mut looks) = (0u64, Vec::new());
            let shown = picture.read(cursor, |_, levels, look| {
                clean += levels[ROWS..].iter().map(|&l| l as u64).sum::<u64>();
                looks.push(look);
            });
            (shown, clean, looks)
        };
        let mut cursor = Cursor::default();
        for _ in 0..8 {
            lanes.step(Engine::Filtering, 100.0);
        }
        let (shown, full, looks) = read(&mut cursor);
        assert_eq!(shown.head, 24);
        assert_eq!(full, 0, "the take opens in a pause: cleaned, nothing");
        assert!(looks.iter().all(|l| *l == Look::Clean));
        // the same pause at Gentle: the room is back, 6 dB down
        let mut gentle = Lanes::new();
        for _ in 0..8 {
            gentle.step(Engine::Filtering, 6.0);
        }
        let mut c = Cursor::default();
        let mut sum = 0u64;
        gentle.picture().read(&mut c, |_, levels, _| {
            sum += levels[ROWS..].iter().map(|&l| l as u64).sum::<u64>();
        });
        assert!(sum > 0);
        // bypass: the original; muted: nothing
        lanes.step(Engine::Bypass, 100.0);
        let (_, clean, looks) = read(&mut cursor);
        assert!(clean > 0 && looks.iter().all(|l| *l == Look::Original));
        lanes.step(Engine::Muted, 100.0);
        assert_eq!(read(&mut cursor).1, 0);
        // frozen at full strength, then at Gentle
        lanes.freeze(100.0);
        lanes.show_frozen(true);
        let (shown, at_full, _) = read(&mut cursor);
        assert!(shown.reset && shown.frozen && shown.head == 30);
        lanes.restrength(6.0);
        let (shown, at_gentle, looks) = read(&mut cursor);
        assert!(shown.reset && at_gentle > at_full);
        assert_eq!(looks[24], Look::Original, "the bypassed column stays");
        lanes.show_frozen(false);
        lanes.clear();
        assert_eq!(read(&mut cursor).0.head, 0);
    }
}
