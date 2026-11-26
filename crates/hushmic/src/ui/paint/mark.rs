//! The HushMic Mark (F1 "Brighten"): four noise bars that brighten towards
//! the voice line lying down after them, its five states, and the morph
//! from unfiltered (standing bars) to filtered (the line).
//!
//! Two drawings of the same mark exist on the boards and both are here:
//! - [`paint_mark`]: AppFx `drawMark` (the Live picture, AppMark, Launch B):
//!   any size, continuous morph, the muted cut-out with round ends.
//! - [`paint_logo`]: the design system's `HushMic.Mark` (rail, About, the
//!   error page): the 16-grid small cut at 20 px and below, the display cut
//!   above, cut-outs with the SVG mask's square ends.
//!
//! egui has no `destination-out`: a cut-out is painted in the ground colour,
//! which is exact because the Mark always sits on `bg` (never over glow).

use eframe::egui::{pos2, vec2, Color32, Painter, Pos2, Shape, Stroke};

use super::super::motion::{c01, ease_out, smooth};
use super::super::tokens::Palette;
use super::mix;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkState {
    Active,
    Bypass,
    Off,
    Muted,
    Error,
}

/// The morph (unfiltered → filtered) in ms: the beam's impact, every later
/// switch-on and Launch B. Back to unfiltered takes two thirds.
pub const MORPH_MS: f32 = 600.0;
pub const UNMORPH_MS: f32 = 400.0;

const NOISE: [[f32; 4]; 4] = [
    [2.0, 5.0, 2.0, 14.0],
    [5.0, 2.0, 2.0, 20.0],
    [8.0, 7.0, 2.0, 10.0],
    [11.0, 4.5, 2.0, 15.0],
];
const EXTRA: [[f32; 2]; 3] = [[14.0, 8.0], [17.0, 5.0], [20.0, 7.5]];
const FADE: [f32; 4] = [0.4, 0.55, 0.72, 0.9];
const FADE_SMALL: [f32; 3] = [0.45, 0.65, 0.88];

/// One frame of the morph at progress `t` (0 unfiltered … 1 filtered):
/// continuous at every size. The line grows from zero inside the first
/// extra bar and only into bars that have lain down; each bar fades out only
/// once it has nearly lain down onto the line.
pub struct MorphFrame {
    /// Per extra bar: (x, y, h, progress, alpha), in units.
    pub extra: [(f32, f32, f32, f32, f32); 3],
    /// The line's length in units (from x = 14).
    pub line: f32,
    /// The line's cooling from ink to the accent.
    pub cool: f32,
}

pub fn morph_frame(t: f32) -> MorphFrame {
    let ps: [f32; 3] = std::array::from_fn(|k| ease_out(c01((t - k as f32 * 0.12) / 0.5)));
    let reach = |p: f32| c01((p - 0.55) / 0.45);
    let line_end = 14.0 + 2.0 * reach(ps[0]) + 3.0 * reach(ps[1]) + 3.0 * reach(ps[2]);
    let extra = std::array::from_fn(|k| {
        let e = EXTRA[k];
        let half = e[1] * (1.0 - ps[k]);
        let a = 1.0 - smooth((ps[k] - 0.9) / 0.1);
        (e[0], 12.0 - half - 1.0, 2.0 * half + 2.0, ps[k], a)
    });
    MorphFrame {
        extra,
        line: line_end - 14.0,
        cool: ease_out(c01((t - 0.6) / 0.4)),
    }
}

/// A rounded rect with radius `r` (clamped to half the short side) as one
/// convex polygon: exact at any size (egui's corner radius is whole points)
/// and one shape, so a translucent bar never blends twice.
pub fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<Pos2> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pts = Vec::with_capacity(28);
    use std::f32::consts::PI;
    let corners = [
        (x + w - r, y + r, -PI / 2.0),
        (x + w - r, y + h - r, 0.0),
        (x + r, y + h - r, PI / 2.0),
        (x + r, y + r, PI),
    ];
    for (cx, cy, a0) in corners {
        if r < 0.01 {
            pts.push(pos2(cx, cy));
            continue;
        }
        for i in 0..=6 {
            let a = a0 + PI / 2.0 * i as f32 / 6.0;
            pts.push(pos2(cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    pts
}

/// Points closer than this to their neighbour are one point.
const SAME: f32 = 0.01;

/// `pts` without the points that repeat their neighbour (the last one
/// against the first too). A bar as wide as its corners are round ends in
/// two arcs that meet in one point, computed twice: a hair apart, the two
/// made an edge of no length whose direction was noise, and the
/// tessellator's soft edge threw the corner far out (the Mark's bars stood
/// as slanted shards at 64 px, where the launch screen shows it).
pub fn tidy(mut pts: Vec<Pos2>) -> Vec<Pos2> {
    pts.dedup_by(|b, a| (*b - *a).length() < SAME);
    while pts.len() > 1 && (pts[pts.len() - 1] - pts[0]).length() < SAME {
        pts.pop();
    }
    pts
}

pub(crate) fn fill(p: &Painter, pts: Vec<Pos2>, c: Color32) {
    let pts = tidy(pts);
    if c != Color32::TRANSPARENT && pts.len() >= 3 {
        p.add(Shape::convex_polygon(pts, c, Stroke::NONE));
    }
}

/// A segment from `a` to `b`, `w` wide, with round (`round = true`) or
/// square-cut ends, as one convex polygon.
pub fn capsule(a: Pos2, b: Pos2, w: f32, round: bool) -> Vec<Pos2> {
    let d = (b - a).normalized();
    let n = vec2(-d.y, d.x) * (w / 2.0);
    if !round {
        return vec![a + n, b + n, b - n, a - n];
    }
    let r = w / 2.0;
    let base = n.y.atan2(n.x);
    let mut pts = Vec::with_capacity(26);
    use std::f32::consts::PI;
    for i in 0..=12 {
        let ang = base - PI * i as f32 / 12.0;
        pts.push(b + vec2(ang.cos(), ang.sin()) * r);
    }
    for i in 0..=12 {
        let ang = base + PI - PI * i as f32 / 12.0;
        pts.push(a + vec2(ang.cos(), ang.sin()) * r);
    }
    pts
}

/// A convex polygon grown by `d` with round corners (an SVG stroke with
/// `stroke-linejoin: round`, plus the fill).
pub fn grow_round(poly: &[Pos2], d: f32) -> Vec<Pos2> {
    let n = poly.len();
    let mut out = Vec::new();
    // orientation: positive area = clockwise in screen coordinates
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let s = if area > 0.0 { 1.0 } else { -1.0 };
    for i in 0..n {
        let prev = poly[(i + n - 1) % n];
        let cur = poly[i];
        let next = poly[(i + 1) % n];
        let e0 = (cur - prev).normalized();
        let e1 = (next - cur).normalized();
        // outward normals
        let n0 = vec2(e0.y, -e0.x) * s;
        let n1 = vec2(e1.y, -e1.x) * s;
        let a0 = n0.y.atan2(n0.x);
        let mut a1 = n1.y.atan2(n1.x);
        // sweep the short way in the polygon's turning direction
        let mut da = a1 - a0;
        while da > std::f32::consts::PI {
            da -= std::f32::consts::TAU;
        }
        while da < -std::f32::consts::PI {
            da += std::f32::consts::TAU;
        }
        a1 = a0 + da;
        for k in 0..=6 {
            let a = a0 + (a1 - a0) * k as f32 / 6.0;
            out.push(cur + vec2(a.cos(), a.sin()) * d);
        }
    }
    out
}

/// AppFx `drawMark`: the Mark's 24-unit box at `top_left`, `u` points per
/// unit (24 px box: u = 1). `t` is the morph (0 unfiltered … 1 filtered);
/// Off draws the filtered Mark at 35 %, Bypass forces t = 0.
pub fn paint_mark(
    p: &Painter,
    top_left: Pos2,
    size: f32,
    t: f32,
    state: MarkState,
    pal: &Palette,
    alpha: f32,
) {
    let u = size / 24.0;
    let mut alpha = alpha;
    let mut t = t;
    match state {
        MarkState::Off => alpha *= 0.35,
        MarkState::Bypass => t = 0.0,
        _ => {}
    }
    let at = |x: f32, y: f32| top_left + vec2(x * u, y * u);
    let f = morph_frame(t);
    for (i, r) in NOISE.iter().enumerate() {
        let o = at(r[0], r[1]);
        fill(
            p,
            rrect(o.x, o.y, r[2] * u, r[3] * u, u),
            pal.ink.gamma_multiply(FADE[i] * alpha),
        );
    }
    for (x, y, h, ps, a) in f.extra {
        if a <= 0.002 {
            continue;
        }
        let o = at(x, y);
        let k = (FADE[3] + (1.0 - FADE[3]) * ps) * a * alpha;
        fill(
            p,
            rrect(o.x, o.y, 2.0 * u, h * u, u),
            pal.ink.gamma_multiply(k),
        );
    }
    if f.line > 0.01 {
        let c = if f.cool >= 1.0 {
            pal.accent
        } else {
            mix(pal.ink, pal.accent, f.cool)
        };
        let o = at(14.0, 11.0);
        fill(
            p,
            rrect(o.x, o.y, f.line * u, 2.0 * u, u),
            c.gamma_multiply(alpha),
        );
    }
    match state {
        MarkState::Muted => {
            fill(
                p,
                capsule(at(2.0, 1.4), at(22.6, 22.4), 4.4 * u, true),
                pal.bg,
            );
            fill(
                p,
                capsule(at(2.8, 2.2), at(21.8, 21.6), 2.0 * u, true),
                pal.danger.gamma_multiply(alpha),
            );
        }
        MarkState::Error => error_badge(
            p,
            top_left,
            u,
            pal,
            alpha,
            4.4,
            1.4,
            [17.35, 16.2, 1.3, 3.2],
            [18.0, 20.6, 0.75],
            [[18.0, 13.6], [22.8, 22.0], [13.2, 22.0]],
        ),
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn error_badge(
    p: &Painter,
    o: Pos2,
    u: f32,
    pal: &Palette,
    alpha: f32,
    cut: f32,
    line: f32,
    bang: [f32; 4],
    dot: [f32; 3],
    tri: [[f32; 2]; 3],
) {
    let tri: Vec<Pos2> = tri.iter().map(|q| o + vec2(q[0] * u, q[1] * u)).collect();
    fill(p, grow_round(&tri, cut * u / 2.0), pal.bg);
    fill(
        p,
        grow_round(&tri, line * u / 2.0),
        pal.danger.gamma_multiply(alpha),
    );
    let b = o + vec2(bang[0] * u, bang[1] * u);
    fill(
        p,
        rrect(b.x, b.y, bang[2] * u, bang[3] * u, bang[2] * u / 3.0),
        pal.bg,
    );
    p.circle_filled(o + vec2(dot[0] * u, dot[1] * u), dot[2] * u, pal.bg);
}

/// The design system's `HushMic.Mark` as a still logo: the small cut at
/// `size` ≤ 20 (rail), the display cut above (About, error page). Never
/// animates: the rail's Mark shows the state, the settle belongs to the
/// Live picture.
pub fn paint_logo(p: &Painter, top_left: Pos2, size: f32, state: MarkState, pal: &Palette) {
    let alpha = if state == MarkState::Off { 0.35 } else { 1.0 };
    if size > 20.0 {
        let t = if state == MarkState::Bypass { 0.0 } else { 1.0 };
        let u = size / 24.0;
        let at = |x: f32, y: f32| top_left + vec2(x * u, y * u);
        // the same body as the canvas drawing, the SVG's cut-outs
        let body = match state {
            MarkState::Bypass => MarkState::Bypass,
            _ => MarkState::Active,
        };
        paint_mark(p, top_left, size, t, body, pal, alpha);
        match state {
            MarkState::Muted => {
                fill(
                    p,
                    capsule(at(2.0, 1.4), at(22.6, 22.4), 4.4 * u, false),
                    pal.bg,
                );
                fill(
                    p,
                    capsule(at(2.8, 2.2), at(21.8, 21.6), 2.0 * u, true),
                    pal.danger,
                );
            }
            MarkState::Error => error_badge(
                p,
                top_left,
                u,
                pal,
                1.0,
                4.4,
                1.4,
                [17.35, 16.2, 1.3, 3.2],
                [18.0, 20.6, 0.75],
                [[18.0, 13.6], [22.8, 22.0], [13.2, 22.0]],
            ),
            _ => {}
        }
        return;
    }
    // small cut, 16 grid
    let u = size / 16.0;
    let at = |x: f32, y: f32| top_left + vec2(x * u, y * u);
    let bar = |x: f32, y: f32, w: f32, h: f32, c: Color32| {
        let o = at(x, y);
        fill(
            p,
            rrect(o.x, o.y, w * u, h * u, w.min(h) * u / 2.0),
            c.gamma_multiply(alpha),
        );
    };
    if state == MarkState::Bypass {
        for r in [
            [1.0, 4.0, 2.0, 8.0],
            [4.0, 1.0, 2.0, 14.0],
            [7.0, 5.0, 2.0, 6.0],
            [10.0, 3.0, 2.0, 10.0],
            [13.0, 5.0, 2.0, 6.0],
        ] {
            bar(r[0], r[1], r[2], r[3], pal.ink);
        }
        return;
    }
    for (i, r) in [
        [0.0, 4.0, 2.0, 8.0],
        [3.0, 1.0, 2.0, 14.0],
        [6.0, 5.0, 2.0, 6.0],
    ]
    .iter()
    .enumerate()
    {
        bar(
            r[0],
            r[1],
            r[2],
            r[3],
            pal.ink.gamma_multiply(FADE_SMALL[i]),
        );
    }
    bar(9.0, 7.0, 7.0, 2.0, pal.accent);
    match state {
        MarkState::Muted => {
            fill(
                p,
                capsule(at(1.0, 0.5), at(15.5, 15.0), 3.2 * u, false),
                pal.bg,
            );
            fill(
                p,
                capsule(at(1.6, 1.1), at(14.9, 14.4), 1.5 * u, true),
                pal.danger,
            );
        }
        MarkState::Error => error_badge(
            p,
            top_left,
            u,
            pal,
            1.0,
            3.4,
            1.1,
            [11.55, 11.1, 0.9, 2.5],
            [12.0, 14.45, 0.55],
            [[12.0, 9.6], [15.4, 15.4], [8.6, 15.4]],
        ),
        _ => {}
    }
}

/// AppMark: the Mark at any size with the morph between unfiltered and
/// filtered (600 ms forward, linear in time because each bar eases itself;
/// 400 ms back, eased). A still Mark asks for no frames.
#[derive(Clone, Copy, Debug)]
pub struct MarkMorph {
    t: f32,
    run: Option<(f64, f32, f32)>,
}

impl MarkMorph {
    pub fn new(filtered: bool) -> MarkMorph {
        MarkMorph {
            t: if filtered { 1.0 } else { 0.0 },
            run: None,
        }
    }

    /// Move towards filtered (`true`) or unfiltered. `instant` for reduced
    /// motion. Returns true when a morph starts.
    pub fn set(&mut self, now: f64, filtered: bool, instant: bool) -> bool {
        let to = if filtered { 1.0 } else { 0.0 };
        let cur = self.value(now);
        if instant {
            self.t = to;
            self.run = None;
            return false;
        }
        if (cur - to).abs() < 1e-4 && self.run.is_none() {
            return false;
        }
        if let Some((_, _, target)) = self.run {
            if target == to {
                return false;
            }
        }
        self.t = cur;
        self.run = Some((now, cur, to));
        true
    }

    /// The morph's progress at `now` (0 unfiltered … 1 filtered).
    pub fn value(&self, now: f64) -> f32 {
        match self.run {
            None => self.t,
            Some((t0, from, to)) => {
                let dur = if to > from { MORPH_MS } else { UNMORPH_MS };
                let k = c01(((now - t0) * 1000.0) as f32 / dur);
                from + (to - from) * if to > from { k } else { ease_out(k) }
            }
        }
    }

    pub fn running(&self, now: f64) -> bool {
        match self.run {
            None => false,
            Some((t0, from, to)) => {
                let dur = if to > from { MORPH_MS } else { UNMORPH_MS };
                ((now - t0) * 1000.0) < dur as f64
            }
        }
    }

    /// Call once per frame: settles a finished run.
    pub fn tick(&mut self, now: f64) -> f32 {
        let v = self.value(now);
        if !self.running(now) {
            self.t = v;
            self.run = None;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bar's round ends meet in one point: it is in the polygon once,
    /// at every size and place the Mark is drawn.
    #[test]
    fn a_bars_outline_has_no_edge_of_no_length() {
        for size in [16.0_f32, 20.0, 24.0, 32.0, 48.0, 64.0, 96.0] {
            let u = size / 24.0;
            for k in 0..200 {
                let (x, y) = (17.3 + k as f32 * 7.77, 3.1 + k as f32 * 3.33);
                for bar in NOISE {
                    let raw = rrect(x + bar[0] * u, y + bar[1] * u, bar[2] * u, bar[3] * u, u);
                    let pts = tidy(raw);
                    assert!(pts.len() >= 3);
                    for i in 0..pts.len() {
                        let d = (pts[(i + 1) % pts.len()] - pts[i]).length();
                        assert!(d >= SAME, "{size} at {x},{y}: an edge {d} long");
                    }
                }
            }
        }
    }

    #[test]
    fn the_morph_is_continuous() {
        // no frame-to-frame jump in the line or a bar's alpha anywhere
        let mut prev = morph_frame(0.0);
        for i in 1..=600 {
            let f = morph_frame(i as f32 / 600.0);
            assert!((f.line - prev.line).abs() < 0.2, "line jumps at {i}");
            for k in 0..3 {
                assert!(
                    (f.extra[k].4 - prev.extra[k].4).abs() < 0.2,
                    "bar {k} pops at {i}"
                );
            }
            prev = f;
        }
        let end = morph_frame(1.0);
        assert!((end.line - 8.0).abs() < 1e-4);
        assert_eq!(end.cool, 1.0);
        assert!(end.extra.iter().all(|e| e.4 <= 0.002));
        let start = morph_frame(0.0);
        assert!(start.line < 0.01);
    }

    #[test]
    fn the_appmark_runs_forward_in_600_and_back_in_400() {
        let mut m = MarkMorph::new(false);
        assert!(m.set(1.0, true, false));
        assert!((m.value(1.3) - 0.5).abs() < 1e-3);
        assert!(m.running(1.59));
        assert!(!m.running(1.61));
        assert_eq!(m.tick(1.61), 1.0);
        assert!(m.set(2.0, false, false));
        assert!(!m.running(2.41));
        assert_eq!(m.tick(2.41), 0.0);
        assert!(!m.set(3.0, true, true));
        assert_eq!(m.value(3.0), 1.0);
    }
}
