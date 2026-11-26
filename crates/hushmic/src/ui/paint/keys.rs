//! Shortcuts' little press and hold diagrams (App-Shortcuts `diagram()`):
//! the key on top (a tap is a 6 px square, a hold a 6 px capsule), what apps
//! hear below (cyan: the cleaned voice, grey: the raw microphone, dotted:
//! silence), 60 px to a step. Static, 132 × 30; and the legend's swatches.

use eframe::egui::{pos2, vec2, Painter, Pos2, Vec2};

use super::super::tokens::Palette;
use super::mark::{capsule, fill, rrect};

/// The diagram's size in points.
pub const SIZE: Vec2 = vec2(132.0, 30.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyDiagram {
    /// Press to mute, press again: clean, silence, clean.
    ToggleMute,
    /// Press for the raw microphone, press again: clean, raw, clean.
    ToggleBypass,
    /// Live only while held: silence, clean, silence.
    PushToTalk,
    /// Silent while held: clean, silence, clean.
    PushToMute,
}

/// What apps hear along a stretch of the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swatch {
    /// The key held down (the legend only).
    KeyHeld,
    /// Apps hear the cleaned voice: 2 px accent, round ends.
    Clean,
    /// The raw microphone: 1.5 px ink-muted, round ends.
    Raw,
    /// Silence: 1 px ink-subtle, 2 on 3 off.
    Silence,
}

const KEY_Y: f32 = 6.0;
const LINE_Y: f32 = 22.0;

/// One diagram with its top-left at `at`.
pub fn diagram(p: &Painter, at: Pos2, kind: KeyDiagram, pal: &Palette) {
    let o = at.to_vec2();
    stroke(p, pos2(2.0, KEY_Y) + o, pos2(130.0, KEY_Y) + o, None, pal);
    let seg = |x1: f32, x2: f32, s: Swatch| {
        stroke(p, pos2(x1, LINE_Y) + o, pos2(x2, LINE_Y) + o, Some(s), pal)
    };
    let tap = |x: f32| {
        fill(
            p,
            rrect(o.x + x - 3.0, o.y + KEY_Y - 3.0, 6.0, 6.0, 1.5),
            pal.ink,
        )
    };
    let hold = |x1: f32, x2: f32| {
        fill(
            p,
            rrect(o.x + x1, o.y + KEY_Y - 3.0, x2 - x1, 6.0, 3.0),
            pal.ink,
        )
    };
    use Swatch::*;
    match kind {
        KeyDiagram::ToggleMute | KeyDiagram::ToggleBypass => {
            let mid = if kind == KeyDiagram::ToggleMute {
                Silence
            } else {
                Raw
            };
            tap(36.0);
            tap(96.0);
            seg(2.0, 36.0, Clean);
            seg(36.0, 96.0, mid);
            seg(96.0, 130.0, Clean);
        }
        KeyDiagram::PushToTalk | KeyDiagram::PushToMute => {
            let (out, held) = if kind == KeyDiagram::PushToTalk {
                (Silence, Clean)
            } else {
                (Clean, Silence)
            };
            hold(40.0, 92.0);
            seg(2.0, 40.0, out);
            seg(40.0, 92.0, held);
            seg(92.0, 130.0, out);
        }
    }
}

/// A legend swatch with its top-left at `at`: 22 × 6 (the key) or 22 × 4.
pub fn swatch(p: &Painter, at: Pos2, kind: Swatch, pal: &Palette) {
    if kind == Swatch::KeyHeld {
        fill(p, rrect(at.x + 1.0, at.y, 20.0, 6.0, 3.0), pal.ink);
    } else {
        stroke(
            p,
            at + vec2(1.0, 2.0),
            at + vec2(21.0, 2.0),
            Some(kind),
            pal,
        );
    }
}

/// The swatch's height (its SVG box).
pub fn swatch_height(kind: Swatch) -> f32 {
    if kind == Swatch::KeyHeld {
        6.0
    } else {
        4.0
    }
}

/// A horizontal SVG `<line>` from `a` to `b`: the key's track (`None`: 1 px
/// `line`, butt ends) or one of what apps hear.
fn stroke(p: &Painter, a: Pos2, b: Pos2, kind: Option<Swatch>, pal: &Palette) {
    match kind {
        None => fill(p, capsule(a, b, 1.0, false), pal.line),
        Some(Swatch::Clean) => fill(p, capsule(a, b, 2.0, true), pal.accent),
        Some(Swatch::Raw) => fill(p, capsule(a, b, 1.5, true), pal.ink_muted),
        Some(Swatch::Silence) => {
            // stroke-dasharray="2 3", butt ends, from the line's start
            let mut x = a.x;
            while x < b.x {
                let e = (x + 2.0).min(b.x);
                fill(
                    p,
                    capsule(pos2(x, a.y), pos2(e, a.y), 1.0, false),
                    pal.ink_subtle,
                );
                x += 5.0;
            }
        }
        Some(Swatch::KeyHeld) => {}
    }
}
