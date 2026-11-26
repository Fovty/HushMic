//! The light layer: one non-interactive painter over the whole window, at
//! egui's Middle order (above the page's widgets, below menus, popups and
//! tooltips). It draws the window's lit top edge and the first-start beam;
//! both are additive in dark, so they can cross text and borders.
//!
//! Every page shows one light at the top while HushMic filters, centred on
//! the window (round 3: the same spot on every page): the static lit edge
//! (`.hm-app-horizon.is-on`). The light has one place: the Live picture
//! hands over its moments each frame ([`super::live::LiveView::paint`]), and
//! a moment enters at the window's centre, where the resting light sits,
//! and spreads from there into that same profile. The first-start beam
//! falls straight down onto the Mark from the top edge right above it (it
//! never crosses text on a slant); its bright spot there fades while the
//! light spreads from the centre, so the light comes to rest at the centre
//! without a jump.
//! The window's top edge is the top of the content, under the desktop's
//! title bar.

use eframe::egui::{self, pos2, Color32, Context, LayerId, Order, Pos2, Rect, Shape};

use super::super::motion::{c01, ease_out, quad_in, smooth};
use super::live::{DRAIN, FALL, SPLASH_IN, SPLASH_OUT};
use super::{alpha, band, image};

/// The lit edge as the Live picture leaves it this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    /// 0..1: how lit (falls during the un-settle).
    pub lit: f32,
    /// The spread from the entry point (0..1) and the cooling to the accent.
    pub front: f32,
    pub cool: f32,
    /// The beam's entry spot (how bright) and where it is: the top edge
    /// right above the Mark, window x.
    pub entry: f32,
    pub entry_x: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beam {
    /// Where it lands (the Mark), window coordinates. It leaves from the
    /// top edge right above it.
    pub x: f32,
    pub y_land: f32,
    /// ms since the moment started, and since the impact.
    pub t_ms: f32,
    pub ti: f32,
}

impl Beam {
    /// The beam this frame in `window`: its tail, its head and its
    /// strength. It falls straight down from the top edge onto the Mark:
    /// the head falls, then the tail drains after it.
    pub fn path(&self, window: Rect) -> (Pos2, Pos2, f32) {
        let t = self.t_ms;
        let (from, to) = (pos2(self.x, window.top()), pos2(self.x, self.y_land));
        let head = if t < FALL { quad_in(t / FALL) } else { 1.0 };
        let tail = if t < FALL {
            0.0
        } else {
            quad_in((t - FALL) / DRAIN)
        };
        let a = if t < FALL {
            0.55 + 0.45 * t / FALL
        } else {
            1.0
        };
        (from.lerp(to, tail), from.lerp(to, head), a)
    }
}

/// What to draw over the window this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LightLayer {
    pub edge: Option<Edge>,
    pub beam: Option<Beam>,
    /// The lit edge at rest (every page while HushMic filters): the boards'
    /// CSS gradient, centred on the window.
    pub static_edge: bool,
}

impl LightLayer {
    pub fn new() -> LightLayer {
        LightLayer::default()
    }

    /// Paint over `window` (the content area). Call once per frame, after
    /// the page.
    pub fn paint(&self, ctx: &Context, window: Rect) {
        let kit = super::super::kit(ctx);
        let pal = kit.pal;
        let p = ctx.layer_painter(LayerId::new(Order::Middle, egui::Id::new("hm-light")));
        let (y, xc) = (window.top(), window.center().x);
        if let Some(e) = self.edge {
            // Live's moments: the light enters at the window's centre (where
            // it rests), then spreads from there until it is the one profile
            // every page shows; the un-settle fades it
            let front = e.front * window.width() / 2.0;
            let mask = |x: f32| {
                let d = (x - xc).abs();
                if e.front > 0.0 {
                    c01((front - d) / 30.0 + 0.5) * e.lit
                } else {
                    0.0
                }
            };
            horizon(&p, &pal, window, &mask, e.cool);
            // where the beam leaves: a bright spot above the Mark, fading
            // while the light spreads from the centre
            if e.entry > 0.0 {
                let ex = e.entry_x;
                let spot = |x: f32| {
                    let d = x - ex;
                    e.entry * (-(d * d) / (2.0 * 10.0 * 10.0)).exp()
                };
                let (l, r) = (ex - 40.0, ex + 40.0);
                if kit.dark() {
                    p.add(Shape::mesh(band(l, r, y, y + 1.0, 2.0, |x| {
                        alpha(Color32::WHITE, 0.95 * spot(x)).additive()
                    })));
                } else {
                    p.add(Shape::mesh(band(l, r, y, y + 1.0, 2.0, |x| {
                        alpha(pal.accent, 0.7 * spot(x))
                    })));
                }
            }
        } else if self.static_edge {
            horizon(&p, &pal, window, &|_| 1.0, 1.0);
        }
        if let (Some(b), true) = (self.beam, kit.dark()) {
            let Some(tex) = kit.tex.as_deref() else {
                return;
            };
            let (tl, h, a) = b.path(window);
            if (h - tl).length() > 1.0 {
                p.add(Shape::mesh(streak(
                    tex.beam.id(),
                    tl,
                    h,
                    24.0,
                    alpha(Color32::WHITE, a).additive(),
                )));
            }
            if b.ti >= 0.0 {
                let s = if b.ti < SPLASH_IN {
                    smooth(b.ti / SPLASH_IN)
                } else {
                    1.0 - ease_out(c01((b.ti - SPLASH_IN) / SPLASH_OUT))
                };
                let k = 0.75 + 0.25 * smooth(b.ti / SPLASH_IN);
                if s > 0.002 {
                    let r = Rect::from_min_max(
                        pos2(b.x - 90.0 * k, b.y_land - 36.0 * k),
                        pos2(b.x + 90.0 * k, b.y_land),
                    );
                    p.add(Shape::mesh(image(
                        tex.splash.id(),
                        r,
                        alpha(Color32::WHITE, 0.85 * s).additive(),
                    )));
                }
            }
        }
    }
}

/// The beam's texture laid along `tail` → `head`, `half` px either side
/// (its v runs tail to head, as the straight drop's rect had it).
fn streak(tex: egui::TextureId, tail: Pos2, head: Pos2, half: f32, c: Color32) -> egui::Mesh {
    let dir = (head - tail).normalized();
    let n = egui::vec2(-dir.y, dir.x) * half;
    let mut m = egui::Mesh::with_texture(tex);
    for (pos, uv) in [
        (tail + n, pos2(0.0, 0.0)),
        (tail - n, pos2(1.0, 0.0)),
        (head - n, pos2(1.0, 1.0)),
        (head + n, pos2(0.0, 1.0)),
    ] {
        m.vertices.push(egui::epaint::Vertex { pos, uv, color: c });
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    m
}

/// `.hm-app-horizon.is-on`, the light at the top of every page while
/// HushMic filters: a 1 px gradient across the window (transparent, line,
/// accent, white at the window's centre, accent, line, transparent) with a
/// 6 px soft light under its middle 60 % (dark, a plain mesh, so software
/// GL draws it too); in light, line to a 70 % accent. `mask` (0..1 per x)
/// reveals it, as Live's moments spread it; `cool` below 1 whitens it while
/// the light arrives.
fn horizon(
    p: &egui::Painter,
    pal: &super::super::Palette,
    window: Rect,
    mask: &dyn Fn(f32) -> f32,
    cool: f32,
) {
    let (x0, w, y) = (window.left(), window.width(), window.top());
    let t = Color32::TRANSPARENT;
    let stops: Vec<(f32, Color32)> = if pal.is_dark() {
        vec![
            (0.0, t),
            (0.08, pal.line),
            (0.38, pal.accent),
            (0.5, Color32::WHITE),
            (0.62, pal.accent),
            (0.92, pal.line),
            (1.0, t),
        ]
    } else {
        let a70 = super::super::tokens::rgba(0, 122, 128, 0.7);
        vec![
            (0.0, t),
            (0.10, pal.line),
            (0.5, a70),
            (0.90, pal.line),
            (1.0, t),
        ]
    };
    let grad = |x: f32| {
        let f = (x - x0) / w;
        for s in stops.windows(2) {
            if f <= s[1].0 {
                let k = c01((f - s[0].0) / (s[1].0 - s[0].0));
                return lerp_premul(s[0].1, s[1].1, k);
            }
        }
        stops[stops.len() - 1].1
    };
    let col = |x: f32| {
        let c = grad(x);
        let m = mask(x);
        if cool < 1.0 && pal.is_dark() {
            // arriving: the middle runs white, cooling to the gradient (the
            // ends stay the line's grey)
            let d = (x - (x0 + w / 2.0)).abs();
            let k = (1.0 - cool) * (1.0 - ease_out(c01(d / (0.38 * w))));
            let white = Color32::WHITE.gamma_multiply(c.a() as f32 / 255.0);
            lerp_premul(c, white, k).gamma_multiply(m)
        } else {
            c.gamma_multiply(m)
        }
    };
    p.add(Shape::mesh(band(x0, x0 + w, y, y + 1.0, 2.0, col)));
    if pal.is_dark() {
        // ::after: radial-gradient(50% 100% at 50% 0%, rgba(255,255,255,.08), transparent)
        let (l, r) = (x0 + 0.2 * w, x0 + 0.8 * w);
        let (cx, rx, ry) = ((l + r) / 2.0, (r - l) / 2.0, 6.0);
        let mut m = egui::Mesh::default();
        let (nx, ny) = (96, 6);
        for j in 0..=ny {
            for i in 0..=nx {
                let x = l + (r - l) * i as f32 / nx as f32;
                let yy = y + 1.0 + 6.0 * j as f32 / ny as f32;
                let d = (((x - cx) / rx).powi(2) + ((yy - y - 1.0) / ry).powi(2)).sqrt();
                m.colored_vertex(
                    pos2(x, yy),
                    alpha(Color32::WHITE, 0.08 * (1.0 - d).max(0.0) * mask(x)),
                );
            }
        }
        let row = nx as u32 + 1;
        for j in 0..ny as u32 {
            for i in 0..nx as u32 {
                let a = j * row + i;
                m.add_triangle(a, a + 1, a + row);
                m.add_triangle(a + 1, a + row + 1, a + row);
            }
        }
        p.add(Shape::mesh(m));
    }
}

/// CSS gradients interpolate in premultiplied sRGB.
fn lerp_premul(a: Color32, b: Color32, k: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_beam_falls_straight_down_onto_the_mark() {
        // Live at 960 with the rail: the Mark sits right of the centre; the
        // beam leaves from the top edge right above it, never on a slant
        let window = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(960.0, 640.0));
        let at = |t_ms| {
            Beam {
                x: 572.0,
                y_land: 185.0,
                t_ms,
                ti: -1.0,
            }
            .path(window)
        };
        let (tail, head, _) = at(0.0);
        assert_eq!((tail, head), (pos2(572.0, 0.0), pos2(572.0, 0.0)));
        for t in [FALL / 4.0, FALL / 2.0, FALL, FALL + DRAIN / 2.0] {
            let (tail, head, _) = at(t);
            assert_eq!((tail.x, head.x), (572.0, 572.0), "{t}");
            assert!(head.y >= tail.y, "{t}");
        }
        let (tail, head, _) = at(FALL / 2.0);
        assert_eq!(tail, pos2(572.0, 0.0));
        assert!(head.y > 0.0 && head.y < 185.0, "{head:?}");
        let (tail, head, _) = at(FALL);
        assert_eq!((tail, head), (pos2(572.0, 0.0), pos2(572.0, 185.0)));
        let (tail, head, _) = at(FALL + DRAIN);
        assert_eq!((tail, head), (pos2(572.0, 185.0), pos2(572.0, 185.0)));
    }
}
