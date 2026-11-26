//! The Live picture (AppLive, Picture A · Lit path) and its moment.
//!
//! One line runs from a mic glyph, through the Mark, to an app glyph, and
//! reads left to right as one stream: what the microphone hears enters at
//! the mic and travels right into the Mark as grey hairlines (newest at the
//! mic end); each column comes out of the Mark as it passes it, cleaned
//! (cyan bars) or as it was (grey, unfiltered), and travels on to the app.
//! The call side is the mic side delayed by the mic side's travel time:
//! nothing comes out before the first column reaches the Mark, a state
//! change takes effect at the Mark (the columns already through keep their
//! look), and when the sound stops the columns lie down.
//! Nothing moves leftwards.
//!
//! The moment (940 ms with the beam, 640 without) runs on the display-rate
//! clock: the beam travels from the top of the window's centre down onto
//! the Mark (first start only), the Mark morphs from unfiltered to
//! filtered, the call line lights from the Mark and the window's lit edge
//! from the window's centre (the light has one place). It only reacts to
//! the state; it
//! never changes it ([`Gate`]).
//!
//! Frames: a still state paints once and asks for nothing; live columns
//! step at 20 Hz while an app uses HushMic (the feed asks for those frames);
//! a moment asks for every display frame while it runs, then nothing.

use std::collections::VecDeque;

use eframe::egui::{pos2, vec2, Color32, Context, Mesh, Painter, Pos2, Rect, Shape};

use super::super::fonts::Type;
use super::super::icons::{self, Icon};
use super::super::motion::{c01, ease_out, smooth, MomentClock};
use super::super::text;
use super::super::tokens::Palette;
use super::super::Kit;
use super::light::{Beam, Edge, LightLayer};
use super::mark::{self, MarkState, MORPH_MS};
use super::{alpha, band, glow_cap, mix, snap, tex_band, GLOW, GLOW_CAP, GLOW_HALF};

// ---------------------------------------------------------------- geometry

/// px per column = one 50 ms meter frame.
pub const PITCH: f32 = 3.0;
/// The clear zone between the Mark's drawing and both lines, in every state.
const CLEAR: f32 = 8.0;
/// The columns keep 12 px further off the Mark, and fade in over 15 px.
const QUIET: f32 = 12.0;
const EMERGE: f32 = 15.0;
/// Every call line ends softly: to 15 % over its last 24 px; the glow over 40.
const TAPER: f32 = 24.0;
const GLOW_TAPER: f32 = 40.0;

// ---------------------------------------------------------------- timeline

/// The beam falls onto the Mark (quad ease-in); everything else starts at
/// its impact.
pub const FALL: f32 = 300.0;
/// After the impact the beam's tail drains into the Mark.
pub const DRAIN: f32 = 220.0;
const LINE_DELAY: f32 = 180.0;
const LINE: f32 = 460.0;
const LINE_COOL: f32 = 200.0;
const EDGE: f32 = 560.0;
const EDGE_COOL: f32 = 220.0;
pub const SPLASH_IN: f32 = 70.0;
pub const SPLASH_OUT: f32 = 480.0;
/// max(MORPH, LINE_DELAY + LINE, EDGE, SPLASH_IN + SPLASH_OUT): 640 ms
/// after the impact.
pub const AFTER: f32 = 640.0;
/// Back to unfiltered: the noise comes back a little faster.
pub const UNSETTLE: f32 = 400.0;
/// When the sound stops (no app, the app corks its stream, the chain
/// stops), the columns lie down; sound back before they lie flat stands
/// them up again, from where they are, within 220 ms.
const LIE: f64 = 0.600;
const RISE: f64 = 0.220;
const STAGE_FADE: f64 = 0.220;
/// Without real stage events the stages arrive at these times (s), as the
/// boards' default.
const AUTO_STAGES: [f64; 3] = [0.15, 0.75, 1.35];
/// On mute, the sound already on its way to the app fades out.
const MUTE_FADE: f64 = 0.200;
/// Columns kept before the first paint has measured the path (more than
/// any window is wide).
const KEEP: usize = 800;

/// What the Live page shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LiveState {
    /// Filtering, in a call.
    On,
    /// Filtering on the Light model for now (looks like On).
    Light,
    /// Filtering, no app uses HushMic yet.
    Ready,
    /// Unfiltered for now (the engine steps down to passthrough).
    Unfiltered,
    Bypass,
    Muted,
    Off,
    Starting,
    Reconnecting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vis {
    Lit,
    Ready,
    Raw,
    Muted,
    Off,
    Starting,
}

fn vis_of(s: LiveState) -> Vis {
    match s {
        LiveState::On | LiveState::Light => Vis::Lit,
        LiveState::Ready => Vis::Ready,
        LiveState::Unfiltered | LiveState::Bypass => Vis::Raw,
        LiveState::Muted => Vis::Muted,
        LiveState::Starting | LiveState::Reconnecting => Vis::Starting,
        LiveState::Off => Vis::Off,
    }
}

fn lit_vis(v: Vis) -> bool {
    matches!(v, Vis::Lit | Vis::Ready)
}

fn sound_vis(v: Vis) -> bool {
    matches!(v, Vis::Lit | Vis::Raw | Vis::Muted)
}

// ---------------------------------------------------------------- the gate

/// What the moment's gate sees.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GateVis {
    Starting,
    Off,
    Filtering,
    Raw,
    Muted,
}

/// What a state change plays.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Play {
    Nothing,
    Beam,
    Morph,
    Unmorph,
    MuteFade,
}

/// The lead's ruling (round 2). The beam belongs to the first start: it
/// falls only when that start (from Starting or Off) ends in filtering. A
/// first start that ends in bypass or mute uses it up, and so does opening
/// the window on a running engine; `beam_used` is persisted once per
/// install (in the window's state file, not the user's config). Unmute is
/// instant. The gate only reacts to the state and has no way to change it,
/// so "never force the state" holds by construction.
#[derive(Clone, Copy, Debug)]
pub struct Gate {
    pub beam_used: bool,
    pub reduced_motion: bool,
    pub light: bool,
}

impl Gate {
    /// The window opened on an engine in `vis`.
    pub fn open(&mut self, vis: GateVis) {
        if !matches!(vis, GateVis::Starting | GateVis::Off) {
            self.beam_used = true;
        }
    }

    pub fn change(&mut self, prev: GateVis, next: GateVis) -> Play {
        let start = matches!(prev, GateVis::Starting | GateVis::Off);
        if start && !matches!(next, GateVis::Starting | GateVis::Off) {
            let beam = next == GateVis::Filtering
                && !self.beam_used
                && !self.reduced_motion
                && !self.light;
            self.beam_used = true;
            if beam {
                return Play::Beam;
            }
        }
        if self.reduced_motion {
            return Play::Nothing;
        }
        match (prev, next) {
            (GateVis::Muted, GateVis::Filtering) => Play::Nothing, // unmute is instant
            (GateVis::Filtering, GateVis::Filtering) => Play::Nothing,
            (_, GateVis::Filtering) => Play::Morph,
            (GateVis::Filtering, GateVis::Raw) => Play::Unmorph,
            (GateVis::Filtering | GateVis::Raw, GateVis::Muted) => Play::MuteFade,
            _ => Play::Nothing,
        }
    }
}

fn gate_vis(v: Vis) -> GateVis {
    match v {
        Vis::Lit | Vis::Ready => GateVis::Filtering,
        Vis::Raw => GateVis::Raw,
        Vis::Muted => GateVis::Muted,
        Vis::Off => GateVis::Off,
        Vis::Starting => GateVis::Starting,
    }
}

// ---------------------------------------------------------------- the moment

#[derive(Clone, Copy, Debug)]
enum AnimKind {
    /// Into filtering. `from`: what the line and the call side show until
    /// the light arrives.
    Settle { from: Vis, beam: bool },
    /// Back to unfiltered.
    Unsettle,
}

#[derive(Clone, Copy, Debug)]
struct Anim {
    kind: AnimKind,
    clock: MomentClock,
}

/// The moment's values at one instant.
#[derive(Clone, Copy, Debug)]
struct Frame {
    mark: f32,
    front: f32,
    cool: f32,
    lit: f32,
    edge_front: f32,
    edge_cool: f32,
    entry: f32,
    beam: Option<f32>,
    ti: f32,
    k: f32,
    active: bool,
    settle: bool,
    unsettle: bool,
}

fn frame_of(anim: Option<&Anim>, vis: Vis, t_ms: Option<f32>) -> Frame {
    let lit = if lit_vis(vis) { 1.0 } else { 0.0 };
    let mut out = Frame {
        mark: lit,
        front: 1.0,
        cool: 1.0,
        lit,
        edge_front: 1.0,
        edge_cool: 1.0,
        entry: 0.0,
        beam: None,
        ti: 1e9,
        k: 1.0,
        active: false,
        settle: false,
        unsettle: false,
    };
    let Some(a) = anim else { return out };
    let t = t_ms.unwrap_or(a.clock.t_ms());
    match a.kind {
        AnimKind::Settle { beam, .. } => {
            let impact = if beam { FALL } else { 0.0 };
            let ti = t - impact;
            let tl = ti - LINE_DELAY;
            out.settle = true;
            out.ti = ti;
            out.lit = 1.0;
            out.mark = c01(ti / MORPH_MS);
            out.front = ease_out(c01(tl / LINE));
            out.cool = ease_out(c01((tl - (LINE - LINE_COOL)) / LINE_COOL));
            out.edge_front = ease_out(c01(ti / EDGE));
            out.edge_cool = ease_out(c01((ti - (EDGE - EDGE_COOL)) / EDGE_COOL));
            if beam {
                out.beam = Some(t);
                // the spot where the beam leaves (above the Mark) hands
                // over to the light spreading from the window's centre:
                // it fades as the spread goes, so the light comes to rest
                // at the centre without a jump
                out.entry = smooth(c01(t / 80.0)) * (1.0 - smooth(c01(ti / EDGE)));
            }
            out.active = t < impact + AFTER;
        }
        AnimKind::Unsettle => {
            let k = ease_out(c01(t / UNSETTLE));
            out.unsettle = true;
            out.mark = 1.0 - k;
            out.lit = 1.0 - k;
            out.k = k;
            out.active = t < UNSETTLE;
        }
    }
    out
}

// ---------------------------------------------------------------- the view

/// How a column comes out of the Mark: the look of the state HushMic is in
/// as it passes, kept all the way to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// Filtered: what apps get, cyan.
    Clean,
    /// Passed through unfiltered (bypass, stepped down): grey.
    Raw,
    /// Muted (or the chain stopped): nothing.
    Silent,
}

fn out_of(v: Vis) -> Look {
    match v {
        Vis::Lit | Vis::Ready => Look::Clean,
        Vis::Raw => Look::Raw,
        Vis::Muted | Vis::Off | Vis::Starting => Look::Silent,
    }
}

/// The columns' height (1 standing, 0 flat) on its way from `from` to `to`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Height {
    t0: f64,
    from: f32,
    to: f32,
    dur: f64,
}

impl Height {
    fn at(&self, now: f64) -> f32 {
        let k = if self.dur > 0.0 {
            ease_out(c01(((now - self.t0) / self.dur) as f32))
        } else {
            1.0
        };
        self.from + (self.to - self.from) * k
    }

    fn done(&self, now: f64) -> bool {
        now - self.t0 >= self.dur
    }
}

/// One 50 ms column of the stream.
#[derive(Clone, Copy, Debug, Default)]
struct Col {
    /// The raw mic and what apps get, 0..1 over 60 dB.
    inp: f32,
    out: f32,
    /// None until the column passes the Mark.
    through: Option<Look>,
    /// The engine's mode when this column entered (meter history only):
    /// the column `nm` older comes out of the Mark with it.
    mode: Option<Look>,
    /// The strength (dB) `out` was cleaned at (None: not known).
    db: Option<f32>,
}

/// A column's cleaned level (RMS, 0..1 over 60 dB) as it would be at `to`
/// dB, cleaned at `from` with this input: the engine's blend after the
/// model, `out' = in + k * (out - in)`, on the column's RMS amplitudes. An
/// approximation (the exact blend needs `in . out` with the input aligned
/// to the output, which the meter does not carry), exact while the output
/// is in step with the input: noise alone (removed, or with the model's
/// trace of it), speech alone. Mixed, it always moves the right way, a
/// little further than the audio: within 1.5 dB getting gentler, 3 dB a
/// step stronger; from Gentle to Maximum what is left is the model's own
/// trace of noise, which the levels do not tell (up to 9 dB, the right
/// way; tested against audio). It stands in only until the columns
/// recorded at the new strength reach the Mark. (Taking speech and noise
/// as independent powers instead does no better, and worse on noise
/// alone.)
pub(crate) fn out_at(inp: f32, out: f32, from: f32, to: f32) -> f32 {
    let Some(k) = crate::miccheck::take::restrength(from, to) else {
        return out;
    };
    let amplitude = |l: f32| {
        if l > 0.0 {
            10f32.powf((l * 60.0 - 60.0) / 20.0)
        } else {
            0.0
        }
    };
    let (i, o) = (amplitude(inp), amplitude(out));
    crate::meter::level((i + k * (o - i)).max(0.0))
}

impl Col {
    /// The level drawn on the call side.
    fn call(&self) -> f32 {
        match self.through {
            Some(Look::Clean) => self.out,
            Some(Look::Raw) => self.inp,
            _ => 0.0,
        }
    }
}

/// The three chain columns the anchors sit over (fr ratios and gap) and the
/// picture's height: `"1 1.6 1"`, gap 0, 168 px on Live (150 compact).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveLayout {
    pub columns: [f32; 3],
    pub gap: f32,
}

impl Default for LiveLayout {
    fn default() -> LiveLayout {
        LiveLayout {
            columns: [1.0, 1.6, 1.0],
            gap: 0.0,
        }
    }
}

/// The Live picture's state machine and painter. Keep one per window (it
/// remembers the stream and whether the beam was used).
pub struct LiveView {
    vis: Vis,
    anim: Option<Anim>,
    /// Newest first: index = 50 ms steps since the column entered at the mic.
    stream: VecDeque<Col>,
    /// How many columns fit on the mic side (up to the Mark) and on the call
    /// side, as the last paint measured the path.
    sides: Option<(usize, usize)>,
    /// Lying down or standing up again (None: standing).
    height: Option<Height>,
    mute_at: Option<f64>,
    stage: Option<[Option<f64>; 3]>,
    auto_stage: Option<f64>,
    gate: Gate,
    playing: bool,
    last_tick: f64,
    /// Review: the moment frozen at this many ms (no frames requested).
    scrub: Option<(f32, bool)>,
    /// The board's `beam="never"`.
    beam_enabled: bool,
    pub layout: LiveLayout,
    /// The name under the mic end (first run, Live); None draws none (the
    /// board's `labels` off).
    pub mic_label: Option<String>,
    /// Who gets the sound, under the app end (Live): the apps' names (true:
    /// an app uses HushMic, the name is ink and medium) or "No app right now".
    pub app_label: Option<(String, bool)>,
    /// Where the last paint put the app label (the page's tooltip).
    pub app_label_rect: Option<Rect>,
    /// No app uses HushMic while the picture plays (Live listens itself):
    /// the app's glyph is dashed, as in [`LiveState::Ready`].
    pub no_app: bool,
    /// Nothing comes in: the mic is silent or unplugged with none standing
    /// in. The mic's name dims and the call side is drawn as silence, never
    /// as a clean voice (UX panel round 2, A13, A16).
    pub quiet_in: bool,
    /// Reduced motion, as the last paint saw it.
    reduced: bool,
    /// The strength (dB) HushMic cleans at now: a column comes out of the
    /// Mark cleaned at it, as mute and bypass come out as they are then.
    strength: Option<f32>,
}

impl LiveView {
    /// A picture opened on `state`. `beam_used` comes from the window's state
    /// file (the beam belongs to the first start after install).
    pub fn new(state: LiveState, beam_used: bool) -> LiveView {
        let vis = vis_of(state);
        let mut gate = Gate {
            beam_used,
            reduced_motion: false,
            light: false,
        };
        gate.open(gate_vis(vis));
        LiveView {
            vis,
            anim: None,
            stream: VecDeque::new(),
            sides: None,
            height: None,
            mute_at: None,
            stage: if vis == Vis::Starting {
                Some([None; 3])
            } else {
                None
            },
            auto_stage: None,
            gate,
            playing: false,
            last_tick: 0.0,
            scrub: None,
            beam_enabled: true,
            layout: LiveLayout::default(),
            mic_label: None,
            app_label: None,
            app_label_rect: None,
            no_app: false,
            quiet_in: false,
            reduced: false,
            strength: None,
        }
    }

    /// The strength (dB) HushMic cleans at now: new columns are recorded
    /// at it, and those reaching the Mark come out cleaned at it. A change
    /// shows at once: the column that just came out takes it too.
    pub fn set_strength(&mut self, db: f32) {
        if self.strength == Some(db) {
            return;
        }
        self.strength = Some(db);
        let Some((nm, _)) = self.sides else {
            return;
        };
        if let Some(c) = self.stream.get_mut(nm) {
            if let (Some(Look::Clean), Some(was)) = (c.through, c.db) {
                c.out = out_at(c.inp, c.out, was, db);
                c.db = Some(db);
            }
        }
    }

    /// The level drawn just right of the Mark (tests).
    #[cfg(test)]
    pub(crate) fn at_mark(&self) -> Option<f32> {
        let (nm, _) = self.sides?;
        self.stream
            .get(nm)
            .filter(|c| c.through.is_some())
            .map(Col::call)
    }

    /// Whether the first-start beam has been used (persist it).
    pub fn beam_used(&self) -> bool {
        self.gate.beam_used
    }

    pub fn set_beam_enabled(&mut self, on: bool) {
        self.beam_enabled = on;
    }

    /// The state changed. Plays what the gate allows; never changes the state.
    pub fn set_state(&mut self, now: f64, state: LiveState, kit: &Kit) -> Play {
        let next = vis_of(state);
        let prev = self.vis;
        if next == prev {
            return Play::Nothing;
        }
        self.vis = next;
        self.gate.reduced_motion = kit.reduced_motion;
        self.gate.light = !kit.dark() || !self.beam_enabled;
        let play = self.gate.change(gate_vis(prev), gate_vis(next));
        if next != Vis::Starting {
            self.stage = None;
            self.auto_stage = None;
        } else if prev != Vis::Starting {
            self.stage = Some([None; 3]);
        }
        if next == Vis::Muted && prev != Vis::Muted {
            if kit.reduced_motion {
                self.silence_call();
            } else {
                self.mute_at = Some(now);
            }
        } else if prev == Vis::Muted && self.mute_at.is_some() {
            self.silence_call();
        }
        self.anim = match play {
            Play::Beam => Some(Anim {
                kind: AnimKind::Settle {
                    from: prev,
                    beam: true,
                },
                clock: MomentClock::new(),
            }),
            Play::Morph => Some(Anim {
                kind: AnimKind::Settle {
                    from: prev,
                    beam: false,
                },
                clock: MomentClock::new(),
            }),
            Play::Unmorph => Some(Anim {
                kind: AnimKind::Unsettle,
                clock: MomentClock::new(),
            }),
            Play::MuteFade | Play::Nothing => None,
        };
        if !sound_vis(next) {
            self.set_playing(now, false, kit);
        }
        play
    }

    /// The starting stages from real events: 1 mic, 2 Mark, 3 app.
    pub fn set_stage(&mut self, now: f64, n: usize) {
        if let Some(st) = self.stage.as_mut() {
            for (i, s) in st.iter_mut().enumerate() {
                if i < n && s.is_none() {
                    *s = Some(now);
                }
            }
        }
    }

    /// Without real stage events, let the stages arrive at 150, 750 and
    /// 1350 ms (the boards' default).
    pub fn auto_stages(&mut self, now: f64) {
        if self.stage.is_some() {
            self.auto_stage = Some(now);
        }
    }

    /// Replay the first start (review aid): the beam if allowed, then the morph.
    pub fn replay_first_start(&mut self, kit: &Kit) {
        self.gate.beam_used = false;
        if lit_vis(self.vis) {
            let beam = self.beam_enabled && kit.dark() && !kit.reduced_motion;
            self.gate.beam_used = true;
            if !kit.reduced_motion {
                self.anim = Some(Anim {
                    kind: AnimKind::Settle {
                        from: Vis::Starting,
                        beam,
                    },
                    clock: MomentClock::new(),
                });
            }
        }
    }

    /// Freeze the switch-on moment at `t_ms` (review and screenshots), with
    /// or without the beam. `None` returns to live.
    pub fn scrub(&mut self, t_ms: Option<f32>, beam: bool) {
        self.scrub = t_ms.map(|t| (t, beam));
    }

    /// An app uses HushMic and sound flows (the columns move). When it stops
    /// (no app, the app corks its stream, the chain stops), the columns lie
    /// down over 600 ms, then the picture is still; under reduced motion they
    /// go at once. Sound back before they lie flat stands them up from where
    /// they are. Silence in a call is no stop: the feed keeps pushing.
    pub fn set_playing(&mut self, now: f64, playing: bool, kit: &Kit) {
        let playing = playing && sound_vis(self.vis);
        if playing == self.playing {
            return;
        }
        self.playing = playing;
        let h = self.height_at(now);
        self.height = None;
        if playing {
            if h <= 0.0 {
                self.stream.clear();
            } else if h < 1.0 && !kit.reduced_motion {
                self.height = Some(Height {
                    t0: now,
                    from: h,
                    to: 1.0,
                    dur: RISE * f64::from(1.0 - h),
                });
            }
        } else if !self.stream.is_empty() {
            if kit.reduced_motion {
                self.stream.clear();
            } else {
                self.height = Some(Height {
                    t0: now,
                    from: h,
                    to: 0.0,
                    dur: LIE * f64::from(h),
                });
            }
        }
    }

    /// The columns' height now: 1 standing, 0 flat.
    fn height_at(&self, now: f64) -> f32 {
        self.height.map_or(1.0, |h| h.at(now))
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    /// One 50 ms meter column: the peaks of five 10 ms hops, 0..1 over 60 dB
    /// (`(dBFS + 60) / 60`), in (raw mic) and out (what apps get).
    /// The column enters at the mic end; `out` shows on the call side only
    /// once the column has travelled there.
    pub fn push(&mut self, now: f64, inp: f32, out: f32) {
        if !self.playing {
            return;
        }
        // a stream that starts with the first filtering starts at the beam's
        // impact, not under the fall
        if self.stream.is_empty() && self.before_impact() {
            return;
        }
        self.step(Col {
            inp,
            out,
            through: None,
            mode: None,
            db: self.strength,
        });
        self.last_tick = now;
    }

    /// The stream moves one column on: `col` enters at the mic, and the
    /// column that reaches the Mark comes out in the state HushMic is in now.
    fn step(&mut self, col: Col) {
        self.stream.push_front(col);
        self.stamp();
        let keep = self.sides.map_or(KEEP, |(m, c)| m + c + 2);
        self.stream.truncate(keep);
    }

    /// Every column past the Mark that has not come out yet comes out now.
    fn stamp(&mut self) {
        let Some((nm, _)) = self.sides else {
            return;
        };
        let now = out_of(self.vis);
        for i in nm..self.stream.len() {
            if self.stream[i].through.is_some() {
                break;
            }
            // it reached the Mark as the column `nm` newer entered: in the
            // engine's mode then (meter history), else the state now
            let look = self.stream[i - nm].mode.unwrap_or(now);
            let c = &mut self.stream[i];
            c.through = Some(look);
            // cleaned at the strength now (a change shows at the Mark at
            // once, not when the columns recorded at it get there)
            if let (Look::Clean, Some(was), Some(to)) = (look, c.db, self.strength) {
                c.out = out_at(c.inp, c.out, was, to);
                c.db = Some(to);
            }
        }
    }

    /// Fill the picture with columns at once (screenshots, tests), oldest
    /// first; those that have reached the Mark come out in the state now.
    pub fn prefill(&mut self, cols: impl IntoIterator<Item = (f32, f32)>, now: f64) {
        let was = self.playing;
        self.playing = true;
        for (i, o) in cols {
            self.push(now, i, o);
        }
        self.playing = was;
    }

    /// The window opens while an app uses HushMic: fill the stream from the
    /// meter's history (oldest first: in, out, and the engine's mode in that
    /// column) so it looks as if it had been running, replacing what the
    /// picture held. Each column past the Mark comes out as the engine was
    /// when it got there; columns that got there after the history's newest
    /// come out in the state now. Call it after `set_playing(now, true)`;
    /// a picture that does not play (ready, off, starting) takes nothing.
    pub fn prefill_history(&mut self, now: f64, cols: impl IntoIterator<Item = (f32, f32, Look)>) {
        if !self.playing {
            return;
        }
        self.stream.clear();
        self.height = None;
        let keep = self.sides.map_or(KEEP, |(m, c)| m + c + 2);
        for (inp, out, look) in cols {
            self.stream.push_front(Col {
                inp,
                out,
                through: None,
                mode: Some(look),
                // the strength now: the best guess for the recent past
                db: self.strength,
            });
            self.stream.truncate(keep);
        }
        self.stamp();
        self.last_tick = now;
    }

    fn before_impact(&self) -> bool {
        match self.anim {
            Some(Anim {
                kind: AnimKind::Settle { beam: true, .. },
                clock,
            }) => clock.t_ms() < FALL,
            _ => false,
        }
    }

    /// What has come out of the Mark goes silent (mute).
    fn silence_call(&mut self) {
        self.mute_at = None;
        for c in self.stream.iter_mut() {
            if c.through.is_some() {
                c.through = Some(Look::Silent);
            }
        }
    }

    /// Nothing in view would change with the next column: the stream
    /// fills the picture, every column draws as the floor (both sides at
    /// or below −60 dBFS, digital silence included) and every column past
    /// the Mark looks the same. Live then looks four times a second
    /// instead of twenty, and a watcher wakes it at the first sound.
    pub fn at_rest(&self) -> bool {
        let Some((m, c)) = self.sides else {
            return false;
        };
        let need = m + c + 2;
        if !self.playing || self.stream.len() < need {
            return false;
        }
        let mut cols = self.stream.iter().take(need);
        let through = self.stream.get(need - 1).and_then(|c| c.through);
        cols.all(|c| c.inp <= 0.0 && c.out <= 0.0 && (c.through.is_none() || c.through == through))
    }

    /// Whether a moment or a fade is running (the view asks for frames).
    pub fn animating(&self, now: f64) -> bool {
        self.anim.is_some()
            || self.height.is_some_and(|h| !h.done(now))
            || self.mute_at.is_some_and(|t| now - t < MUTE_FADE)
            || self.stage_busy(now)
    }

    /// A stage fading in (none under reduced motion: stages just appear).
    /// Waiting for the next one asks for no frames (see [`Self::next_stage_in`]).
    fn stage_busy(&self, now: f64) -> bool {
        !self.reduced
            && self
                .stage
                .is_some_and(|st| st.iter().any(|t| t.is_some_and(|t| now - t < STAGE_FADE)))
    }

    /// Seconds until the next automatic stage arrives (one frame then).
    fn next_stage_in(&self, now: f64) -> Option<f64> {
        let (t0, st) = (self.auto_stage?, self.stage?);
        AUTO_STAGES
            .iter()
            .zip(st)
            .find(|(_, t)| t.is_none())
            .map(|(at, _)| (t0 + at - now).max(0.0))
    }

    fn stage_alphas(&mut self, now: f64, reduced: bool) -> Option<[f32; 3]> {
        if let (Some(t0), Some(st)) = (self.auto_stage, self.stage.as_mut()) {
            for (i, at) in AUTO_STAGES.iter().enumerate() {
                if st[i].is_none() && now - t0 >= *at {
                    st[i] = Some(t0 + at);
                }
            }
        }
        self.stage.map(|st| {
            st.map(|t| match t {
                None => 0.0,
                Some(_) if reduced => 1.0,
                Some(t) => c01(((now - t) / STAGE_FADE) as f32),
            })
        })
    }

    /// Paint the picture into `rect` (its full width and height), and hand the
    /// lit edge and the beam to `light` (drawn over the whole window).
    pub fn paint(&mut self, ctx: &Context, painter: &Painter, rect: Rect, light: &mut LightLayer) {
        let kit = super::super::kit(ctx);
        let now = ctx.input(|i| i.time);
        let ppp = ctx.pixels_per_point();
        // the picture sits on a card (UX panel D1): the Mark's cut-outs are
        // the card's fill, not the page's
        let mut pal = kit.pal;
        pal.bg = pal.card_fill();
        let dark = kit.dark();
        let software = kit.software;

        // the moment's clock: one tick per painted frame
        let scrub = self.scrub;
        if scrub.is_none() {
            if let Some(a) = self.anim.as_mut() {
                a.clock.tick(now);
            }
        }
        let anim_for_frame = match scrub {
            Some((t, beam)) if lit_vis(self.vis) => Some(Anim {
                kind: AnimKind::Settle {
                    from: Vis::Raw,
                    beam: beam && dark,
                },
                clock: MomentClock::at(t),
            }),
            Some(_) => None,
            None => self.anim,
        };
        let a = frame_of(anim_for_frame.as_ref(), self.vis, None);
        if scrub.is_none() && self.anim.is_some() && !a.active {
            self.anim = None;
        }
        if let Some(h) = self.height.filter(|h| h.done(now)) {
            self.height = None;
            if h.to == 0.0 {
                self.stream.clear();
            }
        }
        if self.mute_at.is_some_and(|t| now - t >= MUTE_FADE) {
            self.silence_call();
        }

        // ---- layout: the anchors over the chain columns' centres
        let o = pos2(snap(rect.left(), ppp), snap(rect.top(), ppp));
        let w = rect.width();
        let h = rect.height();
        let fr = self.layout.columns;
        let gap = self.layout.gap;
        let unit = (w - 2.0 * gap) / (fr[0] + fr[1] + fr[2]);
        let x_mic = (fr[0] * unit / 2.0).round();
        let x_mark = (fr[0] * unit + gap + fr[1] * unit / 2.0).round();
        let x_app = ((fr[0] + fr[1]) * unit + 2.0 * gap + fr[2] * unit / 2.0).round();
        let y0 = (h / 2.0).round();
        let mark_l = x_mark - 12.0;
        let (xl, xr) = (mark_l + 22.0 + CLEAR, x_app - 18.0);
        let (xa, xb) = (x_mic + 18.0, mark_l + 2.0 - CLEAR);
        let max_a = (h / 2.0 - 18.0).max(8.0);
        let at = |x: f32, y: f32| o + vec2(x, y);

        let lie = self.height_at(now);
        self.reduced = kit.reduced_motion;
        let st = self.stage_alphas(now, kit.reduced_motion);
        // while a moment paints every frame, the columns glide by the
        // fraction of a column (pixel-rounded); at rest they step at 20 Hz
        let pitch = (PITCH * ppp).round() / ppp;
        // the stream's two sides: columns from the mic end up to the Mark,
        // then from the Mark to the app end
        let (m0, m1) = (xa + 4.0, xb - QUIET);
        let (c0, c1) = (xl + QUIET, xr - 4.0);
        let fit = |len: f32| (len / pitch).floor().max(0.0) as usize + 1;
        let (nm, nc) = (fit(m1 - m0), fit(c1 - c0));
        self.sides = Some((nm, nc));
        self.stamp();
        let glide = if self.playing && a.active && scrub.is_none() && !kit.reduced_motion {
            let n = if software { 2.0 } else { 1.0 };
            (c01(((now - self.last_tick) / (n * 0.05)) as f32) * n as f32 * pitch * ppp).round()
                / ppp
        } else {
            0.0
        };
        let v = self.vis;
        let p = painter;

        // ---- mic glyph and the mic side line
        let g1 = st.map_or(1.0, |s| s[0]);
        if st.is_some() && g1 < 1.0 {
            icons::paint_with(
                p,
                Icon::MicGlyph,
                at(x_mic, y0),
                20.0,
                alpha(pal.ink_subtle, 0.22 * (1.0 - g1)),
                1.5,
                None,
            );
            dotted(
                p,
                at(xa, y0 + 1.0),
                xb - xa,
                alpha(pal.ink_subtle, 0.25 * (1.0 - g1)),
                1.0,
                5.0,
            );
        }
        let mic_c = if v == Vis::Off {
            pal.ink_subtle
        } else {
            pal.ink_muted
        };
        icons::paint_with(
            p,
            Icon::MicGlyph,
            at(x_mic, y0),
            20.0,
            alpha(mic_c, (if v == Vis::Off { 0.5 } else { 1.0 }) * g1),
            1.5,
            None,
        );
        p.rect_filled(
            Rect::from_min_size(at(xa, y0), vec2(xb - xa, 1.0)),
            0.0,
            alpha(
                pal.ink_subtle,
                (if v == Vis::Off { 0.45 } else { 0.7 }) * g1,
            ),
        );

        // ---- mic side: enters at the mic, travels right into the Mark
        if !self.stream.is_empty() && lie > 0.0 {
            for (i, col) in self.stream.iter().take(nm).enumerate() {
                let x = m0 + i as f32 * pitch + glide;
                if x > m1 {
                    break;
                }
                let amp = column(col.inp, max_a, lie);
                if amp < 1.0 {
                    continue;
                }
                let fade = smooth((i as f32 * pitch + glide + pitch) / EMERGE)
                    * (1.0 - smooth((x - (m1 - 24.0)) / 24.0));
                p.rect_filled(
                    Rect::from_min_size(at(x, y0 - amp), vec2(1.0, 2.0 * amp + 1.0)),
                    0.0,
                    alpha(pal.ink_muted, pal.raw_alpha() * fade),
                );
            }
        }

        // ---- the Mark
        let g2 = st.map_or(1.0, |s| s[1]);
        if st.is_some() && g2 < 1.0 {
            mark::paint_mark(
                p,
                at(mark_l, y0 - 12.0),
                24.0,
                0.0,
                MarkState::Bypass,
                &pal,
                0.18 * (1.0 - g2),
            );
        }
        let (kind, mt) = if a.settle || a.unsettle {
            (MarkState::Active, a.mark)
        } else {
            match v {
                Vis::Raw | Vis::Starting => (MarkState::Bypass, a.mark),
                Vis::Off => (MarkState::Off, 1.0),
                Vis::Muted => (MarkState::Muted, 1.0),
                _ => (MarkState::Active, a.mark),
            }
        };
        mark::paint_mark(p, at(mark_l, y0 - 12.0), 24.0, mt, kind, &pal, g2);

        // ---- call side
        let g3 = st.map_or(1.0, |s| s[2]);
        if st.is_some() && g3 < 1.0 {
            dotted(
                p,
                at(xl, y0 + 1.0),
                xr - xl,
                alpha(pal.ink_subtle, 0.25 * (1.0 - g3)),
                1.0,
                5.0,
            );
            icons::paint_with(
                p,
                Icon::AppGlyph,
                at(x_app, y0),
                20.0,
                alpha(pal.ink_subtle, 0.22 * (1.0 - g3)),
                1.5,
                None,
            );
        }
        let from = match anim_for_frame.map(|a| a.kind) {
            Some(AnimKind::Settle { from, .. }) => Some(from),
            _ => None,
        };
        let line = CallLine {
            o,
            y0,
            x0: xl,
            x1: xr,
            pal: &pal,
            dark,
            software,
            g: g3,
            tex: kit.tex.as_deref(),
        };
        line.paint(p, line_vis(v, self.quiet_in), &a, from);

        // ---- call side: the same columns after the Mark, each as it came
        // out of it, travelling on to the app
        let mf = match self.mute_at {
            Some(t) => 1.0 - smooth(((now - t) / MUTE_FADE) as f32),
            None => 1.0,
        };
        if self.stream.len() > nm && lie > 0.0 && v != Vis::Off && mf > 0.0 {
            let span = c1 - c0;
            for (j, col) in self.stream.iter().skip(nm).enumerate() {
                let cx = c0 + j as f32 * pitch + glide;
                if cx > c1 {
                    break;
                }
                let ca = column(col.call(), max_a, lie);
                if ca < 1.0 {
                    continue;
                }
                let f2 = mf
                    * smooth((j as f32 * pitch + glide + pitch) / EMERGE)
                    * (1.0 - smooth((cx - c0 - span * 0.85) / (span * 0.15)));
                let c = if col.through == Some(Look::Raw) {
                    alpha(pal.ink_muted, 0.85 * f2)
                } else {
                    alpha(pal.accent, f2)
                };
                let r = at(cx - 1.0, y0 - ca);
                mark::fill(p, mark::rrect(r.x, r.y, 2.0, 2.0 * ca, 1.0), c);
            }
        }

        let app_c = if v == Vis::Off {
            pal.ink_subtle
        } else {
            pal.ink_muted
        };
        let no_app = v == Vis::Ready || self.no_app && v == Vis::Lit;
        let app_a = match v {
            _ if no_app => 0.45,
            Vis::Off => 0.35,
            Vis::Starting => 0.6,
            _ => 0.9,
        };
        let dash = if no_app { Some([2.0, 2.5]) } else { None };
        icons::paint_with(
            p,
            Icon::AppGlyph,
            at(x_app, y0),
            20.0,
            alpha(app_c, app_a * g3),
            1.5,
            dash,
        );

        // ---- the mic's name under its end (`.hm-applive__label`): 13/18
        // ink-muted, centred on the mic column, 10 px above the bottom.
        // Live names who gets the sound under the app end the same way
        // (`.hm-lv-app`: ink-subtle, or ink and medium while an app uses
        // HushMic), at most 22 % of the width (App-Live3). The mic's name
        // takes the row up to the app's name (24 px apart), and all of it
        // when there is none: it is shortened with "…" only past that (UX
        // panel B19; it was cut at 30 % with half the row empty). Painted
        // only: the page puts the names into the picture's accessible
        // name and the label's tooltip.
        let app = self
            .app_label
            .as_ref()
            .filter(|(n, _)| !n.is_empty())
            .map(|(name, used)| {
                let (ty, c) = if *used {
                    (Type::CAP.medium(), pal.ink)
                } else {
                    (Type::CAP, pal.ink_subtle)
                };
                let b = text::layout_elided(ctx, name, ty, c, APP_LABEL * w);
                let cx = (fr[0] + fr[1] + fr[2] / 2.0) * unit;
                let x = label_x(cx, b.size().x, w);
                (b, x)
            });
        if let Some(name) = self.mic_label.as_deref() {
            let room = mic_label_room(w, app.as_ref().map(|(_, x)| *x));
            let c = if self.quiet_in {
                alpha(pal.ink_subtle, 0.6)
            } else {
                pal.ink_muted
            };
            let b = text::layout_elided(ctx, name, Type::CAP, c, room);
            let cx = fr[0] * unit / 2.0;
            b.paint(p, at(label_x(cx, b.size().x, w), h - 10.0 - b.size().y));
        }
        if let Some((b, x)) = app {
            let r = b.paint(p, at(x, h - 10.0 - b.size().y));
            self.app_label_rect = Some(r);
        } else {
            self.app_label_rect = None;
        }

        // ---- the light over the window: the lit edge and the beam
        let mut lit = a.lit;
        if matches!(v, Vis::Muted | Vis::Off | Vis::Starting) || (v == Vis::Raw && !a.unsettle) {
            lit = 0.0;
        }
        let land = at(mark_l + 13.0, y0);
        if lit > 0.002 || a.entry > 0.0 {
            light.edge = Some(Edge {
                lit,
                front: if a.settle { a.edge_front } else { 1.0 },
                cool: if a.settle { a.edge_cool } else { 1.0 },
                entry: a.entry,
                entry_x: land.x,
            });
        }
        if a.settle {
            if let Some(t) = a.beam {
                if dark {
                    light.beam = Some(Beam {
                        x: land.x,
                        y_land: land.y,
                        t_ms: t,
                        ti: a.ti,
                    });
                }
            }
        }

        // ---- frames: every display frame while a moment or a fade runs
        // (capped near 30 fps on software GL); live columns are the feed's
        // business
        if scrub.is_none() {
            if a.active || self.animating(now) {
                super::super::motion::moment_repaint(ctx);
            } else if let Some(dt) = self.next_stage_in(now) {
                super::super::motion::repaint_in(ctx, std::time::Duration::from_secs_f64(dt));
            }
        }
    }
}

/// An end label centred on `cx`, kept 16 px inside a picture `w` wide (a
/// long name at the narrow size would cross its edge).
fn label_x(cx: f32, lw: f32, w: f32) -> f32 {
    (cx - lw / 2.0).clamp(16.0, (w - 16.0 - lw).max(16.0))
}

/// The app label's widest share of the picture.
pub const APP_LABEL: f32 = 0.22;

/// The widest the mic's name may run in a picture `w` wide: from the 16 px
/// inset to 24 px before the app's name (its left edge `app_x`), or to the
/// far inset when no app is named; never less than the old 30 %.
fn mic_label_room(w: f32, app_x: Option<f32>) -> f32 {
    let end = app_x.map_or(w - 16.0, |x| x - 24.0);
    (end - 16.0).max(0.3 * w)
}

/// The meter scale (0..1 over 60 dB) to a column's 0..1 amplitude.
pub fn amp(l: f32) -> f32 {
    c01((l - 0.28) / 0.62)
}

/// Live's share of the height for the room under speech: the meter's
/// floor (−60 dBFS) up to where [`amp`] starts (−43 dBFS) climbs to this.
const ROOM: f32 = 0.08;

/// Live's 0..1 amplitude: speech as [`amp`] above a low ramp for the
/// room's own noise, so between words the noise coming in still stands
/// on the input side while the cleaned side keeps to the floor.
pub fn live_amp(l: f32) -> f32 {
    let l = c01(l);
    if l < 0.28 {
        ROOM * l / 0.28
    } else {
        ROOM + (1.0 - ROOM) * amp(l)
    }
}

/// A Live column's half-height in px for meter level `l` with `max` px at
/// full scale, `lie` standing (1) to flat (0): never under 1 while it
/// stands, so the floor still shows as a stub and the picture never lies
/// flat for silence; a stream that stops takes its stubs down with it
/// (gone from half way).
fn column(l: f32, max: f32, lie: f32) -> f32 {
    (live_amp(l) * max * lie).round().max(lie.min(1.0).round())
}

/// How the call side's line is drawn: as silence (Muted's dashes) while
/// nothing comes in, whatever HushMic does with it (UX panel round 2, A13,
/// A16); otherwise the state's own look.
fn line_vis(v: Vis, quiet_in: bool) -> Vis {
    match v {
        Vis::Lit | Vis::Ready | Vis::Raw if quiet_in => Vis::Muted,
        v => v,
    }
}

/// AppLive's `dotted()`: `dot`-sized squares every `step` px. One mesh of
/// plain quads: egui draws a rect no wider than its feathering as a line,
/// and a one-pixel square as nothing.
fn dotted(p: &Painter, start: Pos2, len: f32, c: Color32, dot: f32, step: f32) {
    let mut mesh = Mesh::default();
    let mut x = 0.0;
    while x < len {
        let r = Rect::from_min_size(
            pos2(
                (start.x + x).round(),
                start.y - if dot > 1.0 { 1.0 } else { 0.0 },
            ),
            vec2(dot.min(len - x), dot),
        );
        mesh.add_colored_rect(r, c);
        x += step;
    }
    p.add(Shape::mesh(mesh));
}

struct CallLine<'a> {
    o: Pos2,
    y0: f32,
    x0: f32,
    x1: f32,
    pal: &'a Palette,
    dark: bool,
    software: bool,
    g: f32,
    tex: Option<&'a super::tex::Textures>,
}

impl CallLine<'_> {
    fn tail(&self, x: f32) -> f32 {
        1.0 - 0.85 * smooth((x - (self.x1 - TAPER)) / TAPER)
    }

    /// One strip `thick` px tall centred on the line, colour along x.
    fn strip(
        &self,
        p: &Painter,
        xa: f32,
        xb: f32,
        thick: f32,
        step: f32,
        col: impl Fn(f32) -> Color32,
    ) {
        let top = self.o.y + self.y0 - if thick > 1.0 { 1.0 } else { 0.0 };
        let ox = self.o.x;
        p.add(Shape::mesh(band(
            ox + xa,
            ox + xb,
            top,
            top + thick,
            step,
            |x| col(x - ox),
        )));
    }

    fn plain(&self, p: &Painter, c: Color32, a: f32, thick: f32) {
        let g = self.g;
        self.strip(p, self.x0, self.x1 - TAPER, thick, self.x1, |_| {
            alpha(c, a * g)
        });
        self.strip(p, self.x1 - TAPER, self.x1, thick, 1.0, |x| {
            alpha(c, a * g * self.tail(x))
        });
    }

    fn paint(&self, p: &Painter, v: Vis, a: &Frame, from: Option<Vis>) {
        let pal = self.pal;
        let (x0, x1) = (self.x0, self.x1);
        let len = x1 - x0;
        let g = self.g;
        let prev_look = |x: f32| -> (Color32, f32) {
            match from {
                Some(Vis::Off) => (pal.ink_subtle, 0.4),
                Some(Vis::Muted) => (pal.ink_subtle, if (x - x0) % 8.0 < 4.0 { 0.7 } else { 0.0 }),
                Some(Vis::Starting) => (pal.ink_muted, 0.5),
                _ => (pal.ink_muted, 0.7),
            }
        };
        match v {
            Vis::Off => return self.plain(p, pal.ink_subtle, 0.4, 1.0),
            Vis::Starting => return self.plain(p, pal.ink_muted, 0.5, 1.0),
            Vis::Muted => {
                let mut x = x0;
                while x < x1 {
                    let r = Rect::from_min_size(
                        self.o + vec2(x, self.y0),
                        vec2((x1 - x).min(4.0), 1.0),
                    );
                    p.rect_filled(r, 0.0, alpha(pal.ink_subtle, 0.7 * g));
                    x += 8.0;
                }
                return;
            }
            Vis::Raw if !a.unsettle => return self.plain(p, pal.ink_muted, 0.7, 2.0),
            _ => {}
        }
        let dots = v == Vis::Ready;
        let front = if a.settle {
            x0 + a.front * len
        } else {
            x1 + 1.0
        };
        let cool = if a.settle { a.cool } else { 1.0 };
        let arrive = pal.light;
        let line_at = |x: f32| -> (Color32, f32) {
            if a.unsettle {
                return (
                    mix(pal.accent, pal.ink_muted, a.k),
                    (1.0 - 0.3 * a.k) * self.tail(x),
                );
            }
            if x > front {
                let (c, k) = prev_look(x);
                return (c, k * self.tail(x));
            }
            (
                mix(arrive, pal.accent, cool),
                (0.4 + 0.6 * c01((front - x) / 24.0)) * self.tail(x),
            )
        };
        if dots {
            let mut xd = x0 + 2.0;
            while xd < x1 {
                let (c, _) = line_at(xd);
                let da = if xd > front {
                    0.0
                } else if a.settle {
                    0.5 + 0.5 * (1.0 - cool)
                } else {
                    0.5
                } * self.tail(xd);
                if da > 0.0 {
                    let r = Rect::from_min_size(self.o + vec2(xd, self.y0 - 1.0), vec2(2.0, 2.0));
                    p.rect_filled(r, 0.0, alpha(c, da * g));
                }
                xd += 6.0;
            }
            if a.settle && front < x1 {
                self.strip(p, front.max(x0), x1, 2.0, 2.0, |x| {
                    let (c, k) = prev_look(x);
                    alpha(c, k * self.tail(x))
                });
            }
            return;
        }
        if self.dark && !self.software {
            if let Some(tex) = self.tex {
                // the glow strip under the core (additive), the same accent all
                // along the line: it lies around the whole line (26 px, the
                // rules sheet's glow), a blurred cap centred 4 px before its
                // first pixel (drawn from GLOW_CAP before it), fades out over
                // the last 40 px before the app glyph, and is white only
                // while the light arrives (its front soft over 16 px). The
                // core starts at full strength under it.
                let base = GLOW * g * if a.unsettle { 1.0 - a.k } else { 1.0 };
                let end = front.min(x1);
                let along = |x: f32| {
                    let s = glow_cap(x, x0) * (1.0 - smooth((x - (x1 - GLOW_TAPER)) / GLOW_TAPER));
                    if a.settle {
                        s * smooth((front - x) / 16.0)
                    } else {
                        s
                    }
                };
                let ox = self.o.x;
                let (top, bot) = (
                    self.o.y + self.y0 - GLOW_HALF,
                    self.o.y + self.y0 + GLOW_HALF,
                );
                if end > x0 {
                    p.add(Shape::mesh(tex_band(
                        tex.glow.id(),
                        ox + x0 - GLOW_CAP,
                        ox + end,
                        top,
                        bot,
                        1.0,
                        |x| alpha(pal.accent, base * along(x - ox)).additive(),
                    )));
                    if a.settle && cool < 1.0 {
                        p.add(Shape::mesh(tex_band(
                            tex.glow.id(),
                            ox + x0 - GLOW_CAP,
                            ox + end,
                            top,
                            bot,
                            1.0,
                            |x| {
                                alpha(Color32::WHITE, base * (1.0 - cool) * along(x - ox))
                                    .additive()
                            },
                        )));
                    }
                }
            }
        }
        self.strip(p, x0, x1, 2.0, 1.0, |x| {
            let (c, k) = line_at(x);
            alpha(c, k * g)
        });
    }
}

#[cfg(test)]
mod gate_tests {
    use super::{Gate, GateVis::*, Play};

    fn fresh() -> Gate {
        Gate {
            beam_used: false,
            reduced_motion: false,
            light: false,
        }
    }

    #[test]
    fn the_first_start_into_filtering_plays_the_beam_once() {
        let mut g = fresh();
        g.open(Starting);
        assert_eq!(g.change(Starting, Filtering), Play::Beam);
        assert_eq!(g.change(Filtering, Raw), Play::Unmorph);
        assert_eq!(g.change(Raw, Filtering), Play::Morph);
        // off and on again later in the same install: the morph, not the beam
        assert_eq!(g.change(Filtering, Off), Play::Nothing);
        assert_eq!(g.change(Off, Starting), Play::Nothing);
        assert_eq!(g.change(Starting, Filtering), Play::Morph);
    }

    #[test]
    fn a_first_start_in_bypass_or_mute_stays_put_and_uses_the_beam_up() {
        for end in [Raw, Muted] {
            let mut g = fresh();
            g.open(Starting);
            assert_eq!(g.change(Starting, end), Play::Nothing);
            assert!(g.beam_used);
            let back = if end == Muted {
                Play::Nothing
            } else {
                Play::Morph
            };
            assert_eq!(g.change(end, Filtering), back, "{end:?}");
        }
    }

    #[test]
    fn opening_on_a_running_engine_uses_the_beam_up_but_off_does_not() {
        let mut g = fresh();
        g.open(Filtering);
        assert!(g.beam_used);
        let mut g = fresh();
        g.open(Off);
        assert!(!g.beam_used);
        assert_eq!(g.change(Off, Filtering), Play::Beam);
    }

    #[test]
    fn unmute_is_instant_and_mute_fades_the_call_side() {
        let mut g = fresh();
        g.open(Filtering);
        assert_eq!(g.change(Filtering, Muted), Play::MuteFade);
        assert_eq!(g.change(Muted, Filtering), Play::Nothing);
    }

    #[test]
    fn a_later_launch_never_shows_the_beam_again() {
        let mut g = Gate {
            beam_used: true,
            reduced_motion: false,
            light: false,
        };
        g.open(Starting);
        assert_eq!(g.change(Starting, Filtering), Play::Morph);
    }

    #[test]
    fn reduced_motion_and_light_skip_the_beam_but_still_use_it_up() {
        let mut g = Gate {
            beam_used: false,
            reduced_motion: true,
            light: false,
        };
        assert_eq!(g.change(Starting, Filtering), Play::Nothing);
        assert!(g.beam_used);
        let mut g = Gate {
            beam_used: false,
            reduced_motion: false,
            light: true,
        };
        assert_eq!(g.change(Starting, Filtering), Play::Morph);
        assert!(g.beam_used);
    }
}

#[cfg(test)]
mod tests {
    /// UX panel round 2, A13, A16: with nothing coming in, the call side
    /// is silence in every audible state, and only then.
    #[test]
    fn nothing_coming_in_is_drawn_as_silence() {
        use super::{line_vis, Vis};
        for v in [Vis::Lit, Vis::Ready, Vis::Raw] {
            assert_eq!(line_vis(v, true), Vis::Muted, "{v:?}");
            assert_eq!(line_vis(v, false), v, "{v:?}");
        }
        for v in [Vis::Muted, Vis::Off, Vis::Starting] {
            assert_eq!(line_vis(v, true), v, "{v:?}");
        }
    }

    use super::*;

    fn kit() -> Kit {
        Kit::default()
    }

    #[test]
    fn the_mic_name_runs_up_to_the_app_name() {
        // first run step 2 (600 wide, no app): all but the insets
        assert_eq!(mic_label_room(600.0, None), 568.0);
        // Live at 960 (710 wide), "Zoom" at the app end
        assert_eq!(mic_label_room(710.0, Some(640.0)), 600.0);
        // a long app name never squeezes it under the old 30 %
        assert!((mic_label_room(710.0, Some(200.0)) - 213.0).abs() < 0.01);
    }

    #[test]
    fn the_moment_lasts_940_with_the_beam_and_640_without() {
        let mut v = LiveView::new(LiveState::Starting, false);
        assert_eq!(v.set_state(0.0, LiveState::On, &kit()), Play::Beam);
        let a = v.anim.unwrap();
        assert!(frame_of(Some(&a), Vis::Lit, Some(939.0)).active);
        assert!(!frame_of(Some(&a), Vis::Lit, Some(941.0)).active);
        assert_eq!(v.set_state(1.0, LiveState::Bypass, &kit()), Play::Unmorph);
        assert_eq!(v.set_state(2.0, LiveState::On, &kit()), Play::Morph);
        let a = v.anim.unwrap();
        assert!(frame_of(Some(&a), Vis::Lit, Some(639.0)).active);
        assert!(!frame_of(Some(&a), Vis::Lit, Some(641.0)).active);
    }

    #[test]
    fn the_stream_starts_at_the_impact() {
        let k = kit();
        let mut v = LiveView::new(LiveState::Starting, false);
        v.set_state(0.0, LiveState::On, &k);
        v.set_playing(0.0, true, &k);
        v.push(0.05, 0.6, 0.5);
        assert!(v.stream.is_empty(), "columns under the fall are dropped");
        if let Some(a) = v.anim.as_mut() {
            a.clock = MomentClock::at(FALL + 1.0);
        }
        v.push(0.35, 0.6, 0.5);
        assert_eq!(v.stream.len(), 1);
    }

    /// A playing view whose path holds 3 columns up to the Mark and 4 after
    /// it (as a paint measures them).
    fn short(state: LiveState) -> LiveView {
        let mut v = LiveView::new(state, true);
        v.sides = Some((3, 4));
        v.set_playing(0.0, true, &kit());
        v
    }

    /// The call side, nearest the Mark first: each column's drawn level and
    /// how it came out.
    fn call_side(v: &LiveView) -> Vec<(f32, Look)> {
        let nm = v.sides.unwrap().0;
        v.stream
            .iter()
            .skip(nm)
            .map(|c| (c.call(), c.through.unwrap()))
            .collect()
    }

    /// A strength change shows just right of the Mark at once and on
    /// every column coming out after it, not when the columns recorded at
    /// it get there; what came out before keeps its level, a bypassed
    /// column its input.
    #[test]
    fn a_strength_change_comes_out_of_the_mark_at_once() {
        let mut v = short(LiveState::On);
        v.set_strength(12.0);
        // voice at -12 dBFS, cleaned to -24 at 12 dB
        let (inp, out) = (0.8, 0.6);
        for i in 0..6 {
            v.push(i as f64 * 0.05, inp, out);
        }
        let before: Vec<f32> = call_side(&v).iter().map(|c| c.0).collect();
        assert_eq!(before, vec![out; 3]);
        let stronger = out_at(inp, out, 12.0, 24.0);
        assert!(stronger < out - 0.05, "{stronger}");
        v.set_strength(24.0);
        assert_eq!(v.at_mark(), Some(stronger), "at once");
        let side = |v: &LiveView| call_side(v).iter().map(|c| c.0).collect::<Vec<_>>();
        assert_eq!(
            side(&v),
            vec![stronger, out, out],
            "the rest as they came out"
        );
        // the next column out is at it too, recorded at 12 dB
        v.push(0.3, inp, out);
        assert_eq!(side(&v)[..2], [stronger, stronger]);
        // columns recorded at 24 dB come out as they are
        let at24 = 0.4;
        for i in 0..3 {
            v.push(0.35 + i as f64 * 0.05, inp, at24);
        }
        v.push(0.5, inp, at24);
        assert_eq!(v.at_mark(), Some(at24));
        // and back: the way there
        assert!((out_at(inp, stronger, 24.0, 12.0) - out).abs() < 1e-4);
        assert_eq!(out_at(inp, out, 12.0, 12.0), out);
        // bypassed: the input, whatever the strength
        let mut v = short(LiveState::On);
        v.set_strength(12.0);
        v.set_state(0.0, LiveState::Bypass, &kit());
        for i in 0..4 {
            v.push(i as f64 * 0.05, inp, out);
        }
        v.set_strength(24.0);
        assert_eq!(v.at_mark(), Some(inp));
    }

    /// The level at another strength, against audio cleaned at it: speech
    /// and room noise mixed every way, a model that keeps the speech and
    /// leaves none, a trace or more of the noise, measured as the meter
    /// does (RMS of 50 ms). It always moves the right way; gentler within
    /// 1.5 dB, a step stronger within 3; from Gentle to Maximum what is
    /// left is the model's own trace of noise, which the levels do not
    /// tell (the right way only).
    #[test]
    fn a_level_at_another_strength_follows_the_audio() {
        use crate::miccheck::take::alpha;
        let level = |x: &[f32]| {
            let ms = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
            crate::meter::level(ms.sqrt())
        };
        let mut seed = 7u32;
        let n = 2400;
        let z: Vec<f32> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        let db = |l: f32| l * 60.0;
        for (voice, room) in [
            (0.3, 0.05),
            (0.05, 0.1),
            (0.02, 0.05),
            (0.01, 0.1),
            (0.0, 0.1),
            (0.2, 0.0),
        ] {
            for trace in [0.0, 0.02, 0.05] {
                let s: Vec<f32> = (0..n).map(|i| voice * ((i as f32) * 0.07).sin()).collect();
                let inp: Vec<f32> = (0..n).map(|i| s[i] + room * z[i]).collect();
                let m: Vec<f32> = (0..n).map(|i| 0.97 * s[i] + trace * room * z[i]).collect();
                let at = |strength: f32| -> Vec<f32> {
                    let a = alpha(strength);
                    (0..n).map(|i| a * inp[i] + (1.0 - a) * m[i]).collect()
                };
                for (from, to, within) in [
                    (24.0, 6.0, 1.5),
                    (100.0, 6.0, 1.5),
                    (12.0, 6.0, 1.5),
                    (12.0, 24.0, 3.0),
                    (24.0, 100.0, 3.0),
                    (6.0, 100.0, f32::INFINITY),
                ] {
                    let was = level(&at(from));
                    let got = out_at(level(&inp), was, from, to);
                    let want = level(&at(to));
                    let case = format!("{voice}/{room}/{trace} {from}->{to}");
                    assert!(db((got - want).abs()) < within, "{case}: {got} for {want}");
                    if db((want - was).abs()) > 0.5 {
                        assert_eq!((got - was).signum(), (want - was).signum(), "{case}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_call_side_is_the_mic_side_delayed_to_the_mark() {
        let mut v = short(LiveState::On);
        let col = |i: usize| (0.5 + i as f32 * 0.01, 0.3 + i as f32 * 0.01);
        for i in 0..3 {
            let (a, b) = col(i);
            v.push(i as f64 * 0.05, a, b);
        }
        assert!(call_side(&v).is_empty(), "nothing out before the Mark");
        for i in 3..7 {
            let (a, b) = col(i);
            v.push(i as f64 * 0.05, a, b);
            // the column that entered three steps ago comes out, cleaned
            assert_eq!(call_side(&v)[0], (col(i - 3).1, Look::Clean));
            assert_eq!(v.stream[0].inp, col(i).0);
        }
        // the path keeps what it shows, and a little more
        for i in 7..20 {
            v.push(i as f64 * 0.05, 0.5, 0.3);
        }
        assert_eq!(v.stream.len(), 3 + 4 + 2);
    }

    #[test]
    fn a_switch_takes_effect_at_the_mark() {
        let k = kit();
        let mut v = short(LiveState::On);
        for i in 0..5 {
            v.push(i as f64 * 0.05, 0.7, 0.5);
        }
        v.set_state(0.3, LiveState::Bypass, &k);
        v.push(0.35, 0.7, 0.5);
        // the column at the Mark now comes out as it was (its mic level);
        // the two already through stay clean
        assert_eq!(
            call_side(&v),
            [(0.7, Look::Raw), (0.5, Look::Clean), (0.5, Look::Clean)]
        );
        // and back on: the change travels right from the Mark again
        v.set_state(0.4, LiveState::On, &k);
        v.push(0.45, 0.7, 0.5);
        let looks: Vec<Look> = call_side(&v).iter().map(|c| c.1).collect();
        assert_eq!(looks, [Look::Clean, Look::Raw, Look::Clean, Look::Clean]);
    }

    /// A floor stream rests only while every column past the Mark looks
    /// the same: a switch during silence (bypass here; mute and hold to
    /// hear the same) carries its look across the call side at the live
    /// cadence, and the picture rests again once the new look fills it.
    #[test]
    fn a_switch_in_silence_wakes_the_picture_until_its_look_fills_the_call_side() {
        let k = kit();
        let mut v = short(LiveState::On);
        let need = 3 + 4 + 2;
        for i in 0..need {
            v.push(i as f64 * 0.05, 0.0, 0.0);
        }
        assert!(v.at_rest(), "a full floor stream rests");
        v.set_state(0.5, LiveState::Bypass, &k);
        let mut t = 0.55;
        v.push(t, 0.0, 0.0);
        assert!(!v.at_rest(), "the new look is on its way to the app");
        // it rests again only once the new look reaches past the last column
        for _ in 0..need {
            if v.at_rest() {
                break;
            }
            t += 0.05;
            v.push(t, 0.0, 0.0);
        }
        assert!(v.at_rest(), "the call side is all the new look");
        assert!(v
            .stream
            .iter()
            .skip(3)
            .all(|c| c.through == Some(Look::Raw)));
        // any column above the floor wakes it
        v.push(t + 0.05, 0.01, 0.0);
        assert!(!v.at_rest());
    }

    #[test]
    fn mute_silences_what_came_out_and_unmute_is_instant() {
        let k = kit();
        let mut v = short(LiveState::On);
        for i in 0..5 {
            v.push(i as f64 * 0.05, 0.7, 0.5);
        }
        v.set_state(0.3, LiveState::Muted, &k);
        assert!(v.mute_at.is_some(), "what is through fades out");
        v.push(0.35, 0.7, 0.5);
        assert!(v.playing(), "the mic side flows on");
        assert_eq!(
            call_side(&v),
            [(0.0, Look::Silent), (0.5, Look::Clean), (0.5, Look::Clean)]
        );
        v.set_state(0.4, LiveState::On, &k);
        assert!(
            call_side(&v).iter().all(|c| c.0 == 0.0),
            "unmute ends the fade at once"
        );
        v.push(0.45, 0.7, 0.5);
        assert_eq!(call_side(&v)[0], (0.5, Look::Clean));
    }

    #[test]
    fn a_window_opened_mid_call_replays_the_meter_history() {
        let k = kit();
        let state = |m: Look| match m {
            Look::Clean => LiveState::On,
            Look::Raw => LiveState::Bypass,
            Look::Silent => LiveState::Muted,
        };
        use Look::{Clean as C, Raw as R};
        let modes = [C, C, R, R, C, C, C, R, C];
        let col = |i: usize| (0.5 + i as f32 * 0.01, 0.3 + i as f32 * 0.01);
        // the window that was open all along: the state followed the engine
        let mut live = short(LiveState::On);
        for (i, m) in modes.iter().enumerate() {
            live.set_state(i as f64, state(*m), &k);
            live.set_playing(i as f64, true, &k);
            let (a, b) = col(i);
            live.push(i as f64, a, b);
        }
        // the window opened now: the same columns from the meter's history
        let mut opened = short(LiveState::On);
        let hist: Vec<(f32, f32, Look)> = modes
            .iter()
            .enumerate()
            .map(|(i, m)| (col(i).0, col(i).1, *m))
            .collect();
        opened.prefill_history(9.0, hist.iter().copied());
        assert_eq!(call_side(&opened), call_side(&live));
        assert_eq!(opened.stream.len(), live.stream.len());
        // before the first paint has measured the path: the same once it has
        let mut early = LiveView::new(LiveState::On, true);
        early.set_playing(0.0, true, &k);
        early.prefill_history(9.0, hist.iter().copied());
        early.sides = Some((3, 4));
        early.stamp();
        assert_eq!(call_side(&early), call_side(&live));
        // a picture that does not play takes nothing (nothing would move it
        // or lay it down)
        for state in [LiveState::Ready, LiveState::Off, LiveState::Starting] {
            let mut v = LiveView::new(state, true);
            v.set_playing(0.0, true, &k);
            v.prefill_history(0.0, hist.iter().copied());
            assert!(v.stream.is_empty(), "{state:?}");
        }
        // the oldest got to the Mark as the engine muted: silent; the next
        // gets there with a live column: in the state now (bypass)
        let mut v = short(LiveState::Bypass);
        let m = |l: Look| (0.7, 0.5, l);
        v.prefill_history(0.0, [m(C), m(C), m(C), m(Look::Silent)]);
        assert_eq!(call_side(&v), [(0.0, Look::Silent)]);
        v.push(0.05, 0.6, 0.4);
        assert_eq!(call_side(&v), [(0.7, Look::Raw), (0.0, Look::Silent)]);
    }

    #[test]
    fn columns_enter_only_while_the_picture_plays() {
        let k = kit();
        // no app yet: the feed's columns do not enter
        let mut v = LiveView::new(LiveState::On, true);
        v.sides = Some((3, 4));
        v.push(0.0, 0.8, 0.6);
        assert!(v.stream.is_empty());
        // lying down after a stop: nothing new enters or comes out
        v.set_playing(0.0, true, &k);
        for i in 0..4 {
            v.push(i as f64 * 0.05, 0.8, 0.6);
        }
        v.set_playing(0.2, false, &k);
        v.push(0.25, 0.9, 0.7);
        assert_eq!(v.stream.len(), 4);
        assert_eq!(v.stream[0].inp, 0.8);
        assert_eq!(call_side(&v), [(0.6, Look::Clean)]);
    }

    #[test]
    fn the_columns_lie_down_when_the_sound_stops() {
        let k = kit();
        // the app lets go or corks its stream: the whole stream lies down
        let mut v = short(LiveState::On);
        for i in 0..5 {
            v.push(i as f64 * 0.05, 0.8, 0.6);
        }
        v.set_playing(1.0, false, &k);
        assert_eq!(v.height.map(|h| (h.t0, h.to)), Some((1.0, 0.0)));
        assert!(v.animating(1.3));
        assert!(!v.animating(1.7));
        // the same when the call ends (filtering, no app) or the chain stops
        for next in [LiveState::Ready, LiveState::Off, LiveState::Starting] {
            let mut v = short(LiveState::On);
            v.push(0.0, 0.8, 0.6);
            v.set_state(1.0, next, &k);
            assert_eq!(v.height.map(|h| (h.t0, h.to)), Some((1.0, 0.0)), "{next:?}");
        }
        // sound again before it lies flat: it stands up from where it is,
        // within 220 ms, and flows on
        let mut v = short(LiveState::On);
        for i in 0..4 {
            v.push(i as f64 * 0.05, 0.8, 0.6);
        }
        v.set_playing(1.0, false, &k);
        let lain = v.height_at(1.3);
        assert!(lain > 0.0 && lain < 0.2, "{lain}");
        v.set_playing(1.3, true, &k);
        assert_eq!(v.height_at(1.3), lain, "no jump");
        let mut prev = lain;
        for i in 1..=22 {
            let h = v.height_at(1.3 + i as f64 * 0.01);
            assert!(h >= prev, "it only rises");
            prev = h;
        }
        assert_eq!(prev, 1.0);
        assert!(v.animating(1.45) && !v.animating(1.53));
        v.push(1.35, 0.7, 0.5);
        assert_eq!(call_side(&v), [(0.6, Look::Clean), (0.6, Look::Clean)]);
        // and down again while it rises: from where it is, no jump
        let mut v = short(LiveState::On);
        v.push(0.0, 0.8, 0.6);
        v.set_playing(1.0, false, &k);
        v.set_playing(1.3, true, &k);
        let mid = v.height_at(1.4);
        v.set_playing(1.4, false, &k);
        assert_eq!(v.height_at(1.4), mid);
        assert!(v.height_at(1.5) < mid);
        // flat already (the window did not paint): sound starts a new stream
        let mut v = short(LiveState::On);
        v.push(0.0, 0.8, 0.6);
        v.set_playing(1.0, false, &k);
        v.set_playing(2.0, true, &k);
        assert!(v.stream.is_empty() && v.height.is_none());
        // reduced motion: it goes at once
        let mut rm = kit();
        rm.reduced_motion = true;
        let mut v = short(LiveState::On);
        v.push(0.0, 0.7, 0.6);
        v.set_playing(1.0, false, &rm);
        assert!(v.stream.is_empty() && !v.animating(1.0));
    }
}

#[cfg(test)]
mod paint_tests {
    use super::*;
    use crate::ui::{install, Theme};
    use eframe::egui;

    /// The shapes one frame of `view` paints into a 700 × 168 picture.
    fn shapes(view: &mut LiveView) -> Vec<egui::epaint::ClippedShape> {
        let ctx = egui::Context::default();
        install(&ctx, Theme::Dark);
        let out = ctx.run(Default::default(), |ctx| {
            let p = ctx.layer_painter(egui::LayerId::background());
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 168.0));
            view.paint(ctx, &p, rect, &mut LightLayer::default());
        });
        out.shapes
    }

    fn dark() -> egui::Context {
        let ctx = egui::Context::default();
        install(&ctx, Theme::Dark);
        ctx
    }

    /// One frame of `view` at `t` in a 700 × 168 picture: its shapes and the
    /// delay it asked for.
    fn frame(
        ctx: &egui::Context,
        view: &mut LiveView,
        t: f64,
    ) -> (Vec<egui::epaint::ClippedShape>, std::time::Duration) {
        let input = egui::RawInput {
            time: Some(t),
            predicted_dt: 1.0 / 60.0,
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            let p = ctx.layer_painter(egui::LayerId::background());
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 168.0));
            view.paint(ctx, &p, rect, &mut LightLayer::default());
        });
        let d = out.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        (out.shapes, d)
    }

    /// The bars drawn between the Mark and the app glyph.
    fn call_bars(shapes: &[egui::epaint::ClippedShape]) -> usize {
        shapes
            .iter()
            .filter(|s| match &s.shape {
                Shape::Path(p) => {
                    let r = Rect::from_points(&p.points);
                    p.fill.a() > 0 && r.min.x > 370.0 && r.width() <= 2.5
                }
                _ => false,
            })
            .count()
    }

    #[test]
    fn nothing_comes_out_of_the_mark_before_the_first_column_reaches_it() {
        let ctx = dark();
        let mut v = LiveView::new(LiveState::On, true);
        frame(&ctx, &mut v, 0.0);
        let (nm, nc) = v.sides.unwrap();
        // 700 px: the columns from the mic end to the Mark, then to the app
        assert_eq!((nm, nc), (68, 68));
        v.prefill((0..nm).map(|_| (0.8, 0.7)), 0.0);
        assert_eq!(
            call_bars(&frame(&ctx, &mut v, 0.0).0),
            0,
            "the mic side first"
        );
        v.prefill((0..5).map(|_| (0.8, 0.7)), 0.0);
        assert_eq!(call_bars(&frame(&ctx, &mut v, 0.0).0), 5);
    }

    /// The bars' heights between the Mark and the app glyph.
    fn call_heights(shapes: &[egui::epaint::ClippedShape]) -> Vec<f32> {
        shapes
            .iter()
            .filter_map(|s| match &s.shape {
                Shape::Path(p) => {
                    let r = Rect::from_points(&p.points);
                    (p.fill.a() > 0 && r.min.x > 370.0 && r.width() <= 2.5).then(|| r.height())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn silence_keeps_the_floor_and_the_room_stands_over_it() {
        // the floor is a stub, never nothing; the room's noise (−50 dBFS)
        // stands visibly over the cleaned side's floor; speech is no lower
        // than before and full scale is full height
        let max = 66.0;
        assert_eq!(column(0.0, max, 1.0), 1.0);
        let room = column(1.0 / 6.0, max, 1.0);
        assert!(room >= 3.0, "{room}");
        assert!((live_amp(0.28) - ROOM).abs() < 1e-6);
        assert!(live_amp(0.55) >= amp(0.55));
        assert_eq!(live_amp(1.0), 1.0);
        let mut prev = 0.0;
        for i in 0..=100 {
            let a = live_amp(i as f32 / 100.0);
            assert!(a >= prev, "it only rises");
            prev = a;
        }
        // a silent call still draws every column that came out, as stubs
        let ctx = dark();
        let mut v = LiveView::new(LiveState::On, true);
        frame(&ctx, &mut v, 0.0);
        let (nm, _) = v.sides.unwrap();
        v.prefill((0..nm + 5).map(|_| (0.0, 0.0)), 0.0);
        let h = call_heights(&frame(&ctx, &mut v, 0.0).0);
        assert_eq!(h.len(), 5);
        assert!(h.iter().all(|h| (*h - 2.0).abs() < 0.01), "{h:?}");
    }

    #[test]
    fn a_stopped_stream_lies_down_then_asks_for_nothing() {
        let (ctx, k) = (dark(), Kit::default());
        let mut v = LiveView::new(LiveState::On, true);
        frame(&ctx, &mut v, 10.0);
        v.set_playing(10.0, true, &k);
        v.prefill((0..150).map(|_| (0.8, 0.7)), 10.0);
        v.set_playing(10.0, false, &k);
        // every display frame for 600 ms, then nothing (no beat left)
        let (mut t, mut n) = (10.0, 0);
        loop {
            let d = frame(&ctx, &mut v, t).1;
            if d == std::time::Duration::MAX || n > 100 {
                break;
            }
            assert_eq!(d, std::time::Duration::ZERO);
            n += 1;
            t += 1.0 / 60.0;
        }
        assert!((36..=38).contains(&n), "{n} frames");
        assert!(v.stream.is_empty());
        // a stream at the floor lies down too: its stubs stand at first
        // and are gone well before the end (no row of stubs held, then
        // dropped at once)
        let mut v = LiveView::new(LiveState::On, true);
        frame(&ctx, &mut v, 10.0);
        v.set_playing(10.0, true, &k);
        let (nm, _) = v.sides.unwrap();
        v.prefill((0..nm + 20).map(|_| (0.0, 0.0)), 10.0);
        v.set_playing(10.0, false, &k);
        assert_eq!(call_bars(&frame(&ctx, &mut v, 10.0).0), 20);
        assert_eq!(call_bars(&frame(&ctx, &mut v, 10.5).0), 0);
        assert!(!v.stream.is_empty(), "still lying down");
    }

    #[test]
    fn starting_draws_dotted_placeholders_before_its_stages() {
        // AppLive's dotted(): one-pixel dots every 5 px on both sides
        let mut v = LiveView::new(LiveState::Starting, true);
        let dots: usize = shapes(&mut v)
            .iter()
            .filter_map(|s| match &s.shape {
                Shape::Mesh(m)
                    if m.vertices
                        .iter()
                        .all(|x| x.pos.y == 85.0 || x.pos.y == 86.0) =>
                {
                    Some(m.vertices.len() / 4)
                }
                _ => None,
            })
            .sum();
        assert!(dots > 40, "{dots} placeholder dots");
    }

    #[test]
    fn waiting_for_a_stage_asks_for_no_frames() {
        let mut v = LiveView::new(LiveState::Starting, true);
        v.auto_stages(0.0);
        // between stages nothing moves: one frame when the next one is due
        assert!(!v.animating(0.05));
        assert!((v.next_stage_in(0.05).unwrap() - 0.10).abs() < 1e-9);
        let _ = v.stage_alphas(0.16, false);
        assert!(v.animating(0.2), "the first stage fades in");
        assert!(!v.animating(0.4));
        assert!((v.next_stage_in(0.4).unwrap() - 0.35).abs() < 1e-9);
        // reduced motion: stages appear, nothing fades
        v.reduced = true;
        let _ = v.stage_alphas(0.76, true);
        assert!(!v.animating(0.8));
        let _ = v.stage_alphas(1.4, true);
        assert_eq!(v.next_stage_in(1.4), None);
    }

    #[test]
    fn the_mic_end_is_named_only_when_asked() {
        let texts = |v: &mut LiveView| {
            shapes(v)
                .iter()
                .filter_map(|s| match &s.shape {
                    Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let mut v = LiveView::new(LiveState::On, true);
        assert!(texts(&mut v).is_empty());
        v.mic_label = Some("RODE NT-USB".into());
        let t = texts(&mut v);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].0, "RODE NT-USB");
        // centred on the mic column (13.9 % of 700), 10 px above the bottom
        let galley_w = {
            let ctx = egui::Context::default();
            install(&ctx, Theme::Dark);
            crate::ui::text::width(&ctx, "RODE NT-USB", Type::CAP)
        };
        assert!(
            (t[0].1.x + galley_w / 2.0 - 700.0 / 3.6 / 2.0).abs() < 1.0,
            "{t:?}"
        );
        assert!((t[0].1.y - (168.0 - 10.0 - 18.0)).abs() < 1.0, "{t:?}");
    }
}
