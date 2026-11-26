//! The Mic check timeline (AppTake, Take E · Hold to stand).
//!
//! Live: the last 10 seconds scroll by, newest at the right: grey hairlines
//! are what the microphone hears, cyan bars what apps hear, on one lit line,
//! with a faint lane for the comfortable speech range. Replay: the take
//! freezes and a playhead crosses it (10 s for the full width); the part not
//! yet heard is at 50 %. Holding the original makes the grey noise stand up
//! in front of the cyan line in 180 ms (ease-out) and dims the cyan to 25 %;
//! letting go lays it down again. Levels map as on Live's picture (the
//! room's noise stands low under the speech), since Live shows a replay
//! with this view.

use std::collections::VecDeque;

use eframe::egui::{pos2, vec2, Color32, Context, Painter, Rect, Shape};

use super::super::motion::{c01, ease_out, smooth};
use super::live::live_amp as amp;
use super::mark;
use super::{alpha, band, image, mix, tex_band, GLOW, GLOW_HALF};

/// 10 s of 50 ms columns.
pub const COLS: usize = 200;
/// Hold to stand, both directions.
pub const FLIP_MS: f32 = 180.0;
const PAD: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeMode {
    Live,
    Replay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeState {
    /// Filtering: grey noise behind, cyan voice.
    On,
    /// Both layers are the raw sound, in grey.
    Bypass,
    /// Only the mic layer.
    Muted,
    /// A dotted line and nothing captured.
    Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TakeCol {
    /// Raw mic level and what apps hear, 0..1 over 60 dB.
    pub inp: f32,
    pub out: f32,
    /// The out layer is the raw sound (bypass): grey.
    pub raw: bool,
}

pub struct TakeView {
    buf: VecDeque<TakeCol>,
    take: Vec<TakeCol>,
    pub mode: TakeMode,
    pub state: TakeState,
    /// The comfortable speech lane.
    pub band: bool,
    /// Replay playhead 0..1.
    pub position: f32,
    s: f32,
    flip: Option<(f64, f32, f32)>,
}

impl Default for TakeView {
    fn default() -> TakeView {
        TakeView::new()
    }
}

impl TakeView {
    pub fn new() -> TakeView {
        TakeView {
            buf: VecDeque::with_capacity(COLS + 1),
            take: Vec::new(),
            mode: TakeMode::Live,
            state: TakeState::On,
            band: true,
            position: 0.0,
            s: 0.0,
            flip: None,
        }
    }

    /// One 50 ms column (live mode).
    pub fn push(&mut self, inp: f32, out: f32) {
        let col = match self.state {
            TakeState::Bypass => TakeCol {
                inp,
                out: inp,
                raw: true,
            },
            TakeState::Muted => TakeCol {
                inp,
                out: 0.0,
                raw: false,
            },
            _ => TakeCol {
                inp,
                out,
                raw: false,
            },
        };
        self.buf.push_back(col);
        while self.buf.len() > COLS {
            self.buf.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.take.clear();
    }

    /// Live → Replay freezes the last 10 s as the take, playhead at 0.
    pub fn set_mode(&mut self, mode: TakeMode) {
        if mode == TakeMode::Replay && self.mode != TakeMode::Replay {
            self.take = self.buf.iter().copied().collect();
            self.position = 0.0;
        }
        self.mode = mode;
    }

    /// A frozen take restored after the window was re-created.
    pub fn set_take(&mut self, cols: Vec<TakeCol>) {
        self.take = cols;
    }

    pub fn take(&self) -> &[TakeCol] {
        &self.take
    }

    /// Hold to stand: `original` while the hold button or Space is down.
    pub fn set_listen(&mut self, now: f64, original: bool, instant: bool) {
        let to = if original { 1.0 } else { 0.0 };
        let cur = self.listen_value(now);
        if instant {
            self.s = to;
            self.flip = None;
            return;
        }
        if (cur - to).abs() < 1e-4 && self.flip.is_none() {
            return;
        }
        if self.flip.is_some_and(|f| f.2 == to) {
            return;
        }
        self.flip = Some((now, cur, to));
    }

    fn listen_value(&self, now: f64) -> f32 {
        match self.flip {
            None => self.s,
            Some((t0, from, to)) => {
                from + (to - from) * ease_out(c01(((now - t0) * 1000.0) as f32 / FLIP_MS))
            }
        }
    }

    pub fn animating(&self, now: f64) -> bool {
        self.flip
            .is_some_and(|f| ((now - f.0) * 1000.0) < FLIP_MS as f64)
    }

    /// The playhead position for a click at `x` inside `rect` (seek).
    pub fn position_at(&self, rect: Rect, x: f32) -> f32 {
        c01((x - rect.left() - PAD) / (rect.width() - 2.0 * PAD))
    }

    pub fn paint(&mut self, ctx: &Context, p: &Painter, rect: Rect) {
        let kit = super::super::kit(ctx);
        let now = ctx.input(|i| i.time);
        let pal = kit.pal;
        let dark = kit.dark();
        let s = self.listen_value(now);
        if self.flip.is_some() && !self.animating(now) {
            self.s = s;
            self.flip = None;
        } else if self.flip.is_some() {
            super::super::motion::moment_repaint(ctx);
        }
        let o = rect.min;
        let (w, h) = (rect.width(), rect.height());
        let y0 = (h / 2.0).round();
        let max_a = h / 2.0 - 14.0;
        let (x0, len) = (PAD, w - 2.0 * PAD);
        let pitch = len / COLS as f32;
        let st = self.state;
        let replay = self.mode == TakeMode::Replay;
        // a take shorter than 10 s ends at the right too (the moment Replay
        // was pressed); the playhead crosses the take itself
        let late = COLS.saturating_sub(self.take.len()) as f32;
        let px = if replay {
            let n = self.take.len().min(COLS) as f32;
            Some((x0 + (late + self.position * n) * pitch).round())
        } else {
            None
        };
        let at = |x: f32, y: f32| o + vec2(x, y);

        // the comfortable speech lane
        if self.band && st != TakeState::Off {
            let a1 = (amp(0.8) * max_a).round();
            let a2 = (amp(0.55) * max_a).round();
            let col = |x: f32| {
                alpha(
                    pal.ink,
                    0.035 * smooth((x - o.x - x0).min(o.x + x0 + len - x) / 60.0),
                )
            };
            p.add(Shape::mesh(band(
                o.x + x0,
                o.x + x0 + len,
                o.y + y0 - a1,
                o.y + y0 - a2,
                6.0,
                col,
            )));
            p.add(Shape::mesh(band(
                o.x + x0,
                o.x + x0 + len,
                o.y + y0 + 1.0 + a2,
                o.y + y0 + 1.0 + a1,
                6.0,
                col,
            )));
        }

        // the line
        if st == TakeState::Off {
            let mut x = x0;
            while x < x0 + len {
                p.rect_filled(
                    Rect::from_min_size(at(x, y0), vec2(2.0, 1.0)),
                    0.0,
                    alpha(pal.ink_subtle, 0.5),
                );
                x += 6.0;
            }
        } else {
            let line_c = match st {
                TakeState::Bypass => pal.ink_muted,
                TakeState::Muted => pal.ink_subtle,
                _ => mix(pal.accent, pal.ink_subtle, s),
            };
            if dark && !kit.software && st == TakeState::On && s < 0.99 {
                if let Some(tex) = kit.tex.as_deref() {
                    // the lit line's glow, as Live draws it
                    let c = alpha(pal.accent, (1.0 - s) * GLOW).additive();
                    p.add(Shape::mesh(tex_band(
                        tex.glow.id(),
                        o.x + x0,
                        o.x + x0 + len,
                        o.y + y0 - GLOW_HALF,
                        o.y + y0 + GLOW_HALF,
                        len,
                        |_| c,
                    )));
                }
            }
            p.rect_filled(
                Rect::from_min_size(at(x0, y0 - 1.0), vec2(len, 2.0)),
                0.0,
                alpha(line_c, if st == TakeState::Muted { 0.6 } else { 0.9 }),
            );
        }

        // columns
        let cols: Vec<TakeCol> = if replay {
            self.take.clone()
        } else {
            self.buf.iter().copied().collect()
        };
        let n = cols.len();
        let col_x = |i: usize| {
            if replay {
                x0 + (late + i as f32 + 0.5) * pitch
            } else {
                x0 + len - ((n - 1 - i) as f32 + 0.5) * pitch
            }
        };
        let dim = |x: f32| {
            if px.is_some_and(|px| x > px) {
                0.5
            } else {
                1.0
            }
        };
        let age = |i: usize| {
            if replay {
                1.0
            } else {
                1.0 - smooth((((n - 1 - i) as f32) * pitch - len * 0.9) / (len * 0.1))
            }
        };
        let draw_raw = |p: &Painter| {
            for (i, c) in cols.iter().enumerate() {
                let x = col_x(i).round();
                let a = (amp(c.inp) * max_a * (0.9 + 0.1 * s)).round();
                if a < 1.0 || x < x0 {
                    continue;
                }
                p.rect_filled(
                    Rect::from_min_size(at(x, y0 - a), vec2(1.0, 2.0 * a + 1.0)),
                    0.0,
                    alpha(
                        pal.ink_muted,
                        (pal.raw_alpha() + (0.9 - pal.raw_alpha()) * s) * dim(x) * age(i),
                    ),
                );
            }
        };
        let draw_clean = |p: &Painter| {
            for (i, c) in cols.iter().enumerate() {
                let x = col_x(i).round();
                let a = (amp(c.out) * max_a).round();
                if a < 1.0 || x < x0 {
                    continue;
                }
                let (rgb, k) = if c.raw {
                    (pal.ink_muted, 0.85)
                } else {
                    (pal.accent, 1.0)
                };
                let r = at(x - 1.0, y0 - a);
                mark::fill(
                    p,
                    mark::rrect(r.x, r.y, 2.0, 2.0 * a, 1.0),
                    alpha(rgb, k * (1.0 - 0.75 * s) * dim(x) * age(i)),
                );
            }
        };
        if st != TakeState::Off {
            if s < 0.5 {
                draw_raw(p);
                draw_clean(p);
            } else {
                draw_clean(p);
                draw_raw(p);
            }
        }

        // the playhead: a white hairline with a soft 12 px light (ink, no
        // light, in the light theme)
        if let Some(px) = px {
            if dark && !kit.software {
                if let Some(tex) = kit.tex.as_deref() {
                    let r = Rect::from_min_size(at(px - 12.0, 6.0), vec2(24.0, h - 12.0));
                    p.add(Shape::mesh(image(
                        tex.head.id(),
                        r,
                        Color32::WHITE.additive(),
                    )));
                }
            }
            let c = if dark { Color32::WHITE } else { pal.ink };
            p.rect_filled(
                Rect::from_min_size(pos2(o.x + px, o.y + 6.0), vec2(1.0, h - 12.0)),
                0.0,
                alpha(c, 0.95),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_to_stand_flips_in_180_ms_both_ways() {
        let mut t = TakeView::new();
        t.set_listen(1.0, true, false);
        assert!(t.animating(1.17));
        assert!(!t.animating(1.19));
        assert!((t.listen_value(1.19) - 1.0).abs() < 1e-4);
        t.s = 1.0;
        t.flip = None;
        t.set_listen(2.0, false, false);
        assert!(t.listen_value(2.09) < 1.0);
        t.set_listen(3.0, true, true);
        assert_eq!(t.listen_value(3.0), 1.0);
    }

    #[test]
    fn a_short_take_ends_where_replay_was_pressed() {
        let ctx = Context::default();
        crate::ui::install(&ctx, crate::ui::Theme::Dark);
        let mut t = TakeView::new();
        t.mode = TakeMode::Replay;
        t.band = false;
        // 5 s of sound: the right half of the width
        t.set_take(vec![
            TakeCol {
                inp: 0.8,
                out: 0.7,
                raw: false
            };
            COLS / 2
        ]);
        t.position = 0.0;
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(432.0, 138.0));
        let out = ctx.run(Default::default(), |ctx| {
            let p = ctx.layer_painter(eframe::egui::LayerId::background());
            t.paint(ctx, &p, rect);
        });
        let xs: Vec<f32> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                Shape::Rect(r) if r.rect.width() <= 1.0 && r.rect.height() > 4.0 => {
                    Some(r.rect.left())
                }
                _ => None,
            })
            .collect();
        // grey columns from the middle to the right end, and the playhead
        // (the last thin rect) at the take's start, mid-width
        let mid = PAD + (432.0 - 2.0 * PAD) / 2.0;
        let head = *xs.last().unwrap();
        assert!((head - mid).abs() <= 1.0, "{head} vs {mid}");
        let cols = &xs[..xs.len() - 1];
        assert!(cols.iter().all(|x| *x >= mid - 1.0), "{cols:?}");
        assert!(cols.iter().any(|x| *x > 432.0 - PAD - 4.0), "{cols:?}");
    }

    #[test]
    fn replay_freezes_the_last_ten_seconds() {
        let mut t = TakeView::new();
        for i in 0..250 {
            t.push(i as f32 / 250.0, 0.5);
        }
        t.set_mode(TakeMode::Replay);
        assert_eq!(t.take().len(), COLS);
        t.push(0.9, 0.9);
        assert_eq!(
            t.take().len(),
            COLS,
            "the take does not move while replaying"
        );
    }
}
