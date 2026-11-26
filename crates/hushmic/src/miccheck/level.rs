//! The level advice under the timeline: is the microphone too quiet, too
//! loud, silent, or fine? Judged per 50 ms column on the raw side, over
//! speech only.
//!
//! A column counts as speech when it stands [`DYNAMICS_DB`] above the raw
//! noise floor (the quietest tenth of the last 10 s). While HushMic
//! filters, the clean side must also keep the raw level (within
//! [`VOICE_KEPT_DB`]): the engine passes voice at about its raw level, so
//! a knock or a burst of noise it removes is not speech. When the engine
//! visibly removes the floor (by [`DISCRIMINATES_DB`] and more), that
//! test tells voice from noise on its own, and voice only a little above
//! a loud floor counts too. At a gentle strength, in the passthrough
//! tier, or while the light model leaves much of the noise, the clean
//! side keeps noise as well and only the dynamics decide: steady noise is
//! never mistaken for a voice.
//!
//! Thresholds follow the parked A/B rework's calibration (its "active
//! speech level" floor of -42 dBFS is about -36 dBFS for the loud tenth of
//! speech used here) and the design's band (speech should reach into
//! -27..-12 dBFS RMS). A verdict holds until the evidence moves past a
//! hysteresis margin, and lapses to waiting after 30 s without speech.

use super::take::ColumnStats;
use super::types::{Engine, Level, LevelVerdict};
use crate::meter::level;
use std::collections::VecDeque;

/// The clean side within this of the raw side: voice (while filtering).
pub const VOICE_KEPT_DB: f32 = 9.0;
/// Speech stands above the raw floor by this much.
pub const DYNAMICS_DB: f32 = 12.0;
/// The engine removes this much from the floor: its clean side tells
/// voice from noise.
pub const DISCRIMINATES_DB: f32 = 15.0;
/// Speech above the floor by this much is enough then.
const DISCRIMINATED_DYNAMICS_DB: f32 = 6.0;
/// Columns quieter than this are never speech.
const MIN_SPEECH_DB: f32 = -55.0;
/// Speech columns needed before any verdict (1 s).
const MIN_SPEECH: usize = 20;
/// Speech columns the verdict looks at (the last 5 s of speech).
const SPEECH_KEEP: usize = 100;
/// No speech for this many columns (30 s): back to waiting.
const EXPIRE: u64 = 600;
/// Columns for the raw floor (10 s).
const FLOOR_KEEP: usize = 200;
const FLOOR_MIN: usize = 20;
/// Filtering columns among the floor's quiet ones needed to judge the
/// engine's removal.
const REMOVAL_MIN: usize = 10;
/// The loud tenth of speech below this: very quiet (clears above the
/// second value).
pub const QUIET_DB: f32 = -36.0;
const QUIET_CLEAR_DB: f32 = -33.0;
/// The loud tenth of speech above this: too loud (clears below).
pub const LOUD_DB: f32 = -6.0;
const LOUD_CLEAR_DB: f32 = -9.0;
/// Clipping: this many separate incidents within `CLIP_WINDOW` columns,
/// or one incident longer than `CLIP_SUSTAINED` columns; clears after
/// `CLIP_CLEAR` columns without any.
const CLIP_INCIDENTS: usize = 3;
const CLIP_WINDOW: u64 = 100;
const CLIP_SUSTAINED: u32 = 12;
const CLIP_CLEAR: u64 = 160;

#[derive(Clone, Copy)]
struct FloorCol {
    raw_db: f32,
    clean_db: f32,
    filtering: bool,
}

#[derive(Default)]
pub struct LevelCheck {
    col: u64,
    verdict: LevelVerdict,
    speech: VecDeque<f32>,
    last_speech: Option<u64>,
    floor: VecDeque<FloorCol>,
    /// The raw floor (quietest tenth), once there are enough columns.
    floor_db: Option<f32>,
    /// How much the engine removes from the floor's columns (median, over
    /// those recorded while filtering).
    removal_db: Option<f32>,
    incidents: VecDeque<u64>,
    clip_run: u32,
    last_clip: Option<u64>,
    overloaded: bool,
    silent: bool,
    p90: Option<f32>,
}

impl LevelCheck {
    pub fn new() -> LevelCheck {
        LevelCheck::default()
    }

    /// Start over (a new microphone, or the page came back).
    pub fn reset(&mut self) {
        *self = LevelCheck::default();
    }

    /// One completed column, the engine mode it was recorded in, and the
    /// raw side's current run of exact zeros.
    pub fn feed(&mut self, s: &ColumnStats, engine: Engine, zero_run: u64) {
        self.col += 1;
        self.silent = zero_run >= super::take::SILENT_FRAMES;
        self.clipping(s.clipped);
        self.floor.push_back(FloorCol {
            raw_db: s.raw_db,
            clean_db: s.clean_db,
            filtering: engine == Engine::Filtering,
        });
        if self.floor.len() > FLOOR_KEEP {
            self.floor.pop_front();
        }
        self.measure_floor();
        if self.is_speech(s, engine) {
            self.speech.push_back(s.raw_db);
            if self.speech.len() > SPEECH_KEEP {
                self.speech.pop_front();
            }
            self.last_speech = Some(self.col);
        }
        if self
            .last_speech
            .is_none_or(|t| self.col.saturating_sub(t) > EXPIRE)
        {
            self.speech.clear();
            self.last_speech = None;
        }
        self.judge();
    }

    fn measure_floor(&mut self) {
        if self.floor.len() < FLOOR_MIN {
            self.floor_db = None;
            self.removal_db = None;
            return;
        }
        let mut raw: Vec<f32> = self.floor.iter().map(|c| c.raw_db).collect();
        raw.sort_by(f32::total_cmp);
        self.floor_db = Some(raw[raw.len() / 10]);
        // The quiet third is the noise the engine works on.
        let quiet = raw[raw.len() * 3 / 10];
        let mut removed: Vec<f32> = self
            .floor
            .iter()
            .filter(|c| c.filtering && c.raw_db <= quiet)
            .map(|c| c.raw_db - c.clean_db)
            .collect();
        self.removal_db = (removed.len() >= REMOVAL_MIN).then(|| {
            removed.sort_by(f32::total_cmp);
            removed[removed.len() / 2]
        });
    }

    fn is_speech(&self, s: &ColumnStats, engine: Engine) -> bool {
        let Some(floor) = self.floor_db else {
            return false;
        };
        if s.raw_db < MIN_SPEECH_DB {
            return false;
        }
        match engine {
            Engine::Filtering => {
                let kept = s.clean_db >= MIN_SPEECH_DB && s.clean_db >= s.raw_db - VOICE_KEPT_DB;
                let above = if self.removal_db.is_some_and(|r| r >= DISCRIMINATES_DB) {
                    DISCRIMINATED_DYNAMICS_DB
                } else {
                    DYNAMICS_DB
                };
                kept && s.raw_db >= floor + above
            }
            _ => s.raw_db >= floor + DYNAMICS_DB,
        }
    }

    fn clipping(&mut self, clipped: bool) {
        if clipped {
            if self.clip_run == 0 {
                self.incidents.push_back(self.col);
            }
            self.clip_run += 1;
            self.last_clip = Some(self.col);
        } else {
            self.clip_run = 0;
        }
        while self
            .incidents
            .front()
            .is_some_and(|&t| self.col - t > CLIP_WINDOW)
        {
            self.incidents.pop_front();
        }
        if self.clip_run > CLIP_SUSTAINED || self.incidents.len() >= CLIP_INCIDENTS {
            self.overloaded = true;
        } else if self.last_clip.is_none_or(|t| self.col - t > CLIP_CLEAR) {
            self.overloaded = false;
        }
    }

    fn judge(&mut self) {
        self.p90 = (self.speech.len() >= MIN_SPEECH).then(|| {
            let mut v: Vec<f32> = self.speech.iter().copied().collect();
            v.sort_by(f32::total_cmp);
            v[(v.len() * 9) / 10]
        });
        let by_level = match self.p90 {
            None => LevelVerdict::Waiting,
            Some(p) => match self.verdict {
                LevelVerdict::TooLoud if p > LOUD_CLEAR_DB => LevelVerdict::TooLoud,
                LevelVerdict::VeryQuiet if p < QUIET_CLEAR_DB => LevelVerdict::VeryQuiet,
                _ if p > LOUD_DB => LevelVerdict::TooLoud,
                _ if p < QUIET_DB => LevelVerdict::VeryQuiet,
                _ => LevelVerdict::Good,
            },
        };
        self.verdict = if self.overloaded {
            LevelVerdict::TooLoud
        } else {
            by_level
        };
    }

    pub fn level(&self) -> Level {
        if self.silent {
            return Level {
                verdict: LevelVerdict::NoSound,
                speech: None,
            };
        }
        Level {
            verdict: self.verdict,
            speech: self.p90.map(|db| level(10f32.powf(db / 20.0))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(raw_db: f32, clean_db: f32) -> ColumnStats {
        ColumnStats {
            raw_db,
            clean_db,
            raw_peak: 10f32.powf(raw_db / 20.0) * 1.4,
            clipped: false,
        }
    }

    /// A take: `speech` columns of voice at `voice_db` between noise at
    /// `noise_db` (removed by 25 dB while filtering).
    fn talk(c: &mut LevelCheck, engine: Engine, voice_db: f32, noise_db: f32, n: usize) {
        for i in 0..n {
            let s = if i % 3 != 0 {
                col(voice_db, voice_db - 1.0)
            } else {
                col(noise_db, noise_db - 25.0)
            };
            let s = if engine == Engine::Filtering {
                s
            } else {
                ColumnStats {
                    clean_db: s.raw_db,
                    ..s
                }
            };
            c.feed(&s, engine, 0);
        }
    }

    #[test]
    fn waits_for_speech_then_judges_the_loud_tenth() {
        let mut c = LevelCheck::new();
        // Noise only: the engine removes it, nothing counts as speech.
        for _ in 0..200 {
            c.feed(&col(-40.0, -70.0), Engine::Filtering, 0);
        }
        assert_eq!(c.level().verdict, LevelVerdict::Waiting);
        assert_eq!(c.level().speech, None);
        talk(&mut c, Engine::Filtering, -22.0, -45.0, 60);
        let l = c.level();
        assert_eq!(l.verdict, LevelVerdict::Good);
        assert!((l.speech.unwrap() - (60.0 - 22.0) / 60.0).abs() < 1e-3);
    }

    #[test]
    fn quiet_and_loud_speech_with_hysteresis() {
        let mut c = LevelCheck::new();
        talk(&mut c, Engine::Filtering, -40.0, -55.0, 120);
        assert_eq!(c.level().verdict, LevelVerdict::VeryQuiet);
        // A little louder, still inside the hysteresis: stays quiet.
        talk(&mut c, Engine::Filtering, -35.0, -55.0, 200);
        assert_eq!(c.level().verdict, LevelVerdict::VeryQuiet);
        talk(&mut c, Engine::Filtering, -25.0, -55.0, 200);
        assert_eq!(c.level().verdict, LevelVerdict::Good);
        talk(&mut c, Engine::Filtering, -4.0, -40.0, 200);
        assert_eq!(c.level().verdict, LevelVerdict::TooLoud);
        talk(&mut c, Engine::Filtering, -8.0, -40.0, 200);
        assert_eq!(c.level().verdict, LevelVerdict::TooLoud, "hysteresis");
        talk(&mut c, Engine::Filtering, -15.0, -40.0, 200);
        assert_eq!(c.level().verdict, LevelVerdict::Good);
    }

    #[test]
    fn noise_the_engine_keeps_is_not_speech() {
        // Gentle (6 dB), a model that leaves 8 dB, the passthrough tier:
        // noise only, nobody speaking.
        for (raw, clean) in [
            (-45.0, -51.0),
            (-30.0, -36.0),
            (-40.0, -48.0),
            (-35.0, -35.0),
        ] {
            let mut c = LevelCheck::new();
            for i in 0..300 {
                // Steady noise wobbles a little.
                let w = [0.0, 1.5, -1.0, 2.5, -2.0][i % 5];
                c.feed(&col(raw + w, clean + w), Engine::Filtering, 0);
            }
            assert_eq!(
                c.level().verdict,
                LevelVerdict::Waiting,
                "noise at {raw} dBFS, {clean} after the engine"
            );
        }
    }

    #[test]
    fn a_voice_counts_at_every_strength() {
        // Gentle: the noise stays at -51, a voice stands well above it.
        let mut c = LevelCheck::new();
        for i in 0..150 {
            let s = if i % 3 != 0 {
                col(-22.0, -23.0)
            } else {
                col(-45.0, -51.0)
            };
            c.feed(&s, Engine::Filtering, 0);
        }
        assert_eq!(c.level().verdict, LevelVerdict::Good);
        // A loud room the engine clears: a voice 8 dB over the noise.
        let mut c = LevelCheck::new();
        for i in 0..150 {
            let s = if i % 3 != 0 {
                col(-27.0, -28.0)
            } else {
                col(-35.0, -60.0)
            };
            c.feed(&s, Engine::Filtering, 0);
        }
        assert_eq!(c.level().verdict, LevelVerdict::Good);
    }

    #[test]
    fn bypass_judges_speech_by_dynamics() {
        let mut c = LevelCheck::new();
        talk(&mut c, Engine::Bypass, -20.0, -50.0, 150);
        assert_eq!(c.level().verdict, LevelVerdict::Good);
        // Steady loud noise is not speech in bypass either.
        let mut c = LevelCheck::new();
        for _ in 0..300 {
            c.feed(&col(-20.0, -20.0), Engine::Bypass, 0);
        }
        assert_eq!(c.level().verdict, LevelVerdict::Waiting);
    }

    #[test]
    fn clipping_overloads_and_clears() {
        let mut c = LevelCheck::new();
        talk(&mut c, Engine::Filtering, -20.0, -50.0, 60);
        let clipped = ColumnStats {
            clipped: true,
            ..col(-12.0, -12.0)
        };
        // One knock is fine.
        c.feed(&clipped, Engine::Filtering, 0);
        talk(&mut c, Engine::Filtering, -20.0, -50.0, 10);
        assert_eq!(c.level().verdict, LevelVerdict::Good);
        for _ in 0..2 {
            c.feed(&clipped, Engine::Filtering, 0);
            talk(&mut c, Engine::Filtering, -20.0, -50.0, 10);
        }
        assert_eq!(c.level().verdict, LevelVerdict::TooLoud, "three incidents");
        talk(&mut c, Engine::Filtering, -20.0, -50.0, 170);
        assert_eq!(c.level().verdict, LevelVerdict::Good, "cleared after 8 s");
        for _ in 0..13 {
            c.feed(&clipped, Engine::Filtering, 0);
        }
        assert_eq!(c.level().verdict, LevelVerdict::TooLoud, "sustained");
    }

    #[test]
    fn exact_silence_is_no_sound_and_speech_expires() {
        let mut c = LevelCheck::new();
        talk(&mut c, Engine::Filtering, -20.0, -50.0, 60);
        c.feed(&col(-120.0, -120.0), Engine::Filtering, 3 * 48_000);
        assert_eq!(c.level().verdict, LevelVerdict::NoSound);
        c.feed(&col(-50.0, -80.0), Engine::Filtering, 0);
        assert_eq!(c.level().verdict, LevelVerdict::Good, "the sound came back");
        for _ in 0..610 {
            c.feed(&col(-50.0, -80.0), Engine::Filtering, 0);
        }
        assert_eq!(c.level().verdict, LevelVerdict::Waiting, "expired");
        c.reset();
        assert_eq!(c.level(), Level::default());
    }
}
