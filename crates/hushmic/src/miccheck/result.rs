//! The result of a frozen take, in words first and a number second:
//! "Between your words: much quieter (26 dB)", "Your voice: kept".
//!
//! Between the words: the take is aligned, so the pauses can be found on
//! the clean side (columns 30 dB or more under its loud tenth, as the
//! demo clips' documented numbers were measured) and compared with the raw
//! side at the very same moments. The clean side is clamped to a -75 dBFS
//! measurement floor (the engine gates pauses to near digital silence;
//! anything below that is not a meaningful difference). A take without
//! pauses falls back to the two sides' own noise floors.
//!
//! The voice: the distribution-based voice-band comparison of
//! [`super::metrics`], which never reports a number for a take without
//! clear speech.

use super::metrics;
use super::take::{db, FrozenTake};
use super::types::{Engine, Quieter, TakeResult, Voice, COLUMN_FRAMES};
use crate::tr;

/// Pauses lie this far under the clean side's loud tenth.
const PAUSE_BELOW_DB: f32 = 30.0;
/// The engine's gated floor counts as this level.
const FLOOR_DBFS: f32 = metrics::SILENT_FLOOR_DBFS;
/// Columns of pause needed (0.5 s).
const MIN_PAUSE_COLUMNS: usize = 10;
/// Raw columns quieter than this hold nothing to remove.
const RAW_QUIET_DBFS: f32 = -70.0;
/// Voice within this of the original counts as kept.
const KEPT_DB: f32 = -3.0;

fn rms_db(x: &[f32]) -> f32 {
    db(x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len().max(1) as f64)
}

/// The words for a reduction in dB.
pub fn quieter(db: f32) -> Quieter {
    if db >= 20.0 {
        Quieter::Much
    } else if db >= 10.0 {
        Quieter::Clearly
    } else if db >= 3.0 {
        Quieter::Slightly
    } else {
        Quieter::Same
    }
}

/// Paired reduction over the take's pauses, or `None` without enough of
/// them.
fn pause_reduction(take: &FrozenTake) -> Option<f32> {
    let cols: Vec<(f32, f32)> = take
        .raw
        .as_chunks::<COLUMN_FRAMES>()
        .0
        .iter()
        .zip(take.clean.as_chunks::<COLUMN_FRAMES>().0)
        .map(|(r, c)| (rms_db(r), rms_db(c)))
        .collect();
    let mut clean: Vec<f32> = cols.iter().map(|&(_, c)| c).collect();
    if clean.is_empty() {
        return None;
    }
    clean.sort_by(f32::total_cmp);
    let loud = clean[(clean.len() * 9) / 10];
    let pauses: Vec<f32> = cols
        .iter()
        .filter(|&&(r, c)| c < loud - PAUSE_BELOW_DB && r > RAW_QUIET_DBFS)
        .map(|&(r, c)| r - c.max(FLOOR_DBFS))
        .collect();
    (pauses.len() >= MIN_PAUSE_COLUMNS)
        .then(|| (pauses.iter().sum::<f32>() / pauses.len() as f32).max(0.0))
}

/// Evaluate a frozen take. Its columns' own modes decide where they are
/// known; `engine` (the mode at the freeze) where they are not. A take
/// that spans a mode change is judged on the part recorded while
/// filtering, else on the part in bypass. `None` when there is nothing to
/// compare: muted or off (the clean side is silence by choice), or less
/// than two seconds of audio.
pub fn evaluate(take: &FrozenTake, engine: Engine) -> Option<TakeResult> {
    match take.modes.first() {
        Some(&first) if take.modes.len() == take.columns.len() => {
            if take.modes.iter().all(|&m| m == first) {
                return evaluate_in(take, first);
            }
            [Engine::Filtering, Engine::Bypass]
                .into_iter()
                .find_map(|mode| {
                    take.only(mode)
                        .filter(|t| t.secs() >= 2.0)
                        .and_then(|t| evaluate_in(&t, mode))
                })
        }
        _ => evaluate_in(take, engine),
    }
}

fn evaluate_in(take: &FrozenTake, engine: Engine) -> Option<TakeResult> {
    if matches!(engine, Engine::Muted | Engine::Off) || take.secs() < 2.0 {
        return None;
    }
    let m = metrics::compute(&take.raw, &take.clean);
    let reduction = match engine {
        // Both sides are the raw microphone: nothing is quieter.
        Engine::Bypass => Some(0.0),
        _ => pause_reduction(take).or(Some(m.background_reduction_db)),
    }
    .map(f32::round);
    let (voice, voice_db) = if m.voice_measurable {
        let d = m.voice_retention_db.round();
        (
            if d >= KEPT_DB {
                Voice::Kept
            } else {
                Voice::Quieter
            },
            Some(d.min(0.0)),
        )
    } else {
        (Voice::NotMeasured, None)
    };
    Some(TakeResult {
        between_words: quieter(reduction.unwrap_or(0.0)),
        between_words_db: reduction,
        voice,
        voice_db,
    })
}

impl TakeResult {
    /// "Much quieter", the words alone.
    pub fn between_words_word(&self) -> String {
        match self.between_words {
            Quieter::Much => tr!("miccheck-quieter-much"),
            Quieter::Clearly => tr!("miccheck-quieter-clearly"),
            Quieter::Slightly => tr!("miccheck-quieter-slightly"),
            Quieter::Same => tr!("miccheck-quieter-same"),
        }
    }

    /// "(26 dB)", or empty without a number.
    pub fn between_words_number(&self) -> String {
        match self.between_words_db {
            Some(d) if self.between_words != Quieter::Same => {
                tr!("miccheck-db", db = format!("{d:.0}"))
            }
            _ => String::new(),
        }
    }
}

impl super::types::LevelVerdict {
    /// "Sounds good", "Very quiet", ...
    pub fn word(self) -> String {
        use super::types::LevelVerdict as L;
        match self {
            L::Waiting => tr!("miccheck-level-waiting"),
            L::Good => tr!("miccheck-level-good"),
            L::VeryQuiet => tr!("miccheck-level-quiet"),
            L::TooLoud => tr!("miccheck-level-loud"),
            L::NoSound => tr!("miccheck-level-silent"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

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

    /// 300 ms tone bursts at `voice` amplitude every 600 ms over noise at
    /// `noise` amplitude; the clean side keeps the bursts times `keep` and
    /// the noise times `left`.
    fn take(voice: f32, noise: f32, keep: f32, left: f32) -> FrozenTake {
        let n = 480_000;
        let mut rng = Lcg(3);
        let mut raw = Vec::with_capacity(n);
        let mut clean = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / 48_000.0;
            let on = (i / 14_400) % 2 == 0;
            let v = if on {
                voice * ((TAU * 220.0 * t).sin() + 0.5 * (TAU * 660.0 * t).sin())
            } else {
                0.0
            };
            let z = noise * rng.next();
            raw.push(v + z);
            clean.push(v * keep + z * left);
        }
        FrozenTake::from_audio(raw, clean)
    }

    #[test]
    fn noise_removed_between_words_and_voice_kept() {
        crate::i18n::pin_english();
        // Noise at -35 dBFS RMS, removed by 30 dB; voice kept.
        let r = evaluate(&take(0.2, 0.031, 1.0, 0.0316), Engine::Filtering).unwrap();
        let db = r.between_words_db.unwrap();
        assert!((db - 30.0).abs() <= 1.0, "{r:?}");
        assert_eq!(r.between_words, Quieter::Much);
        assert_eq!(r.voice, Voice::Kept);
        assert_eq!(r.between_words_word(), "Much quieter");
        assert_eq!(r.between_words_number(), format!("({db:.0} dB)"));
    }

    #[test]
    fn gated_pauses_count_from_the_measurement_floor() {
        // The engine gates the pauses to digital silence: the reduction is
        // raw noise minus -75 dBFS, not minus -120.
        let r = evaluate(&take(0.2, 0.031, 1.0, 0.0), Engine::Filtering).unwrap();
        let db = r.between_words_db.unwrap();
        assert!(
            (db - 40.0).abs() <= 1.0,
            "-35 dBFS noise to the -75 floor: {r:?}"
        );
    }

    #[test]
    fn a_take_across_a_mode_change_is_judged_on_its_filtering_part() {
        // The first 4 s in bypass (clean == raw), the rest filtering.
        let f = take(0.2, 0.031, 1.0, 0.0316);
        let b = take(0.2, 0.031, 1.0, 1.0);
        let split = 80 * COLUMN_FRAMES;
        let mut raw = b.raw[..split].to_vec();
        raw.extend_from_slice(&f.raw[split..]);
        let mut clean = b.clean[..split].to_vec();
        clean.extend_from_slice(&f.clean[split..]);
        let mut mixed = FrozenTake::from_audio(raw, clean);
        mixed.modes = (0..mixed.columns.len())
            .map(|i| {
                if i < 80 {
                    Engine::Bypass
                } else {
                    Engine::Filtering
                }
            })
            .collect();
        let r = evaluate(&mixed, Engine::Bypass).unwrap();
        assert!(
            (r.between_words_db.unwrap() - 30.0).abs() <= 1.5,
            "the filtering part, not the mode at the freeze: {r:?}"
        );
        // Mostly muted with a short filtering tail: nothing to judge.
        mixed.modes = (0..mixed.columns.len())
            .map(|i| {
                if i < 190 {
                    Engine::Muted
                } else {
                    Engine::Filtering
                }
            })
            .collect();
        assert_eq!(evaluate(&mixed, Engine::Filtering), None);
    }

    #[test]
    fn a_ducked_voice_is_reported_and_bypass_is_the_same() {
        let r = evaluate(&take(0.2, 0.031, 0.3, 0.0316), Engine::Filtering).unwrap();
        assert_eq!(r.voice, Voice::Quieter, "{r:?}");
        assert!(r.voice_db.unwrap() <= -9.0, "{r:?}");
        let b = evaluate(&take(0.2, 0.031, 1.0, 1.0), Engine::Bypass).unwrap();
        assert_eq!(
            (b.between_words, b.between_words_db),
            (Quieter::Same, Some(0.0))
        );
        assert_eq!(b.voice, Voice::Kept);
    }

    #[test]
    fn no_speech_and_mute_never_make_up_a_number() {
        crate::i18n::pin_english();
        let r = evaluate(&take(0.0, 0.031, 1.0, 0.0316), Engine::Filtering).unwrap();
        assert_eq!(r.voice, Voice::NotMeasured);
        assert_eq!(r.voice_db, None);
        assert!(evaluate(&take(0.2, 0.031, 0.0, 0.0), Engine::Muted).is_none());
        let short = FrozenTake::from_audio(vec![0.1; 48_000], vec![0.1; 48_000]);
        assert!(evaluate(&short, Engine::Filtering).is_none());
    }

    #[test]
    fn words_follow_the_thresholds() {
        assert_eq!(quieter(26.0), Quieter::Much);
        assert_eq!(quieter(20.0), Quieter::Much);
        assert_eq!(quieter(12.0), Quieter::Clearly);
        assert_eq!(quieter(4.0), Quieter::Slightly);
        assert_eq!(quieter(2.9), Quieter::Same);
    }
}
