//! The painters: the Mark, the Live picture (AppLive), the Mic check
//! timeline (AppTake), Shortcuts' key diagrams, and the light layer over the
//! window (lit edge, beam).
//!
//! Everything maps onto egui primitives as appart drew it for the boards:
//! flat rects and convex polygons, mesh strips with per-vertex colour for
//! colour that changes along a line, textures made once at start-up and
//! drawn stretched, tinted and (in dark) additive. No blur, no filters.

pub mod clips;
pub mod keys;
pub mod light;
pub mod live;
pub mod mark;
pub mod spectro;
pub mod take;
pub mod tex;

use eframe::egui::{epaint::Vertex, pos2, Color32, Mesh, Pos2, Rect, TextureId};

/// sRGB-space mix with rounding, as AppFx's `mix()` + `rgba()`.
pub fn mix(a: Color32, b: Color32, k: f32) -> Color32 {
    let k = k.clamp(0.0, 1.0);
    let [ar, ag, ab, aa] = a.to_srgba_unmultiplied();
    let [br, bg, bb, ba] = b.to_srgba_unmultiplied();
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Color32::from_rgba_unmultiplied(l(ar, br), l(ag, bg), l(ab, bb), l(aa, ba))
}

/// `rgba(colour, a)`: the colour at `a` opacity.
pub fn alpha(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a.clamp(0.0, 1.0))
}

/// Snap a coordinate to the physical pixel grid.
pub fn snap(x: f32, ppp: f32) -> f32 {
    (x * ppp).round() / ppp
}

/// A horizontal band `[x0, x1] × [y0, y1]` whose colour changes along x:
/// one mesh strip, `step` points per segment (AppFx `sliceLine`).
pub fn band(x0: f32, x1: f32, y0: f32, y1: f32, step: f32, col: impl Fn(f32) -> Color32) -> Mesh {
    let mut m = Mesh::default();
    if x1 <= x0 {
        return m;
    }
    let segs = (((x1 - x0) / step).ceil() as usize).max(1);
    for i in 0..=segs {
        let x = (x0 + step * i as f32).min(x1);
        let c = col(x);
        m.colored_vertex(pos2(x, y0), c);
        m.colored_vertex(pos2(x, y1), c);
        if i > 0 {
            let k = i as u32 * 2;
            m.add_triangle(k - 2, k - 1, k);
            m.add_triangle(k - 1, k + 1, k);
        }
    }
    m
}

// The lit line's glow, as the round-3 rules sheet drew it (adopted
// 2026-11-05; design/v1-ux3/NOTES-3b.md, "Glow (adopted)"): an 8 px band
// blurred by a Gaussian of σ 3.5 px across the line, 0.337 at the line,
// half strength 5.1 px off it; its start a blurred edge centred 4 px before
// the line's first pixel. First run's picture and the replay's take draw
// the same glow.

/// The glow's strength: the [`glow_profile`] texture (peak 0.747) drawn at
/// this, so 0.337 at the line.
pub const GLOW: f32 = 0.45;
/// The glow band: the line's centre ± this (26 px).
pub const GLOW_HALF: f32 = 13.0;
/// The glow is drawn from this far before the line's first pixel, where its
/// cap ([`glow_cap`]) is down to 0.011: across the 8 px clear zone after
/// the Mark and 4 px over the Mark's right edge, at most 0.126 of the cap
/// there (about 4 % at the line), so it can't be seen, as the sheet drew
/// it.
pub const GLOW_CAP: f32 = 12.0;
/// σ 3.5 px, times √2 (the erf's scale).
const GLOW_S: f32 = 3.5 * std::f32::consts::SQRT_2;

/// The glow across the line, `d` px from its centre (0..0.747): an 8 px
/// band blurred by a Gaussian of σ 3.5 px.
pub fn glow_profile(d: f32) -> f32 {
    0.5 * (erf((d + 4.0) / GLOW_S) - erf((d - 4.0) / GLOW_S))
}

/// The glow's start at `x` for a line whose first pixel is `x0`: a blurred
/// edge centred 4 px before it (0.874 at x0, 0.5 at x0 − 4, 0.989 at x0 + 4).
pub fn glow_cap(x: f32, x0: f32) -> f32 {
    0.5 * (1.0 + erf((x - (x0 - 4.0)) / GLOW_S))
}

/// The error function (Abramowitz and Stegun 7.1.26, within 1.5e-7).
pub fn erf(x: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_8) * t - 0.284_496_74) * t
            + 0.254_829_6)
            * t
            * (-x * x).exp();
    y.copysign(x)
}

/// The same with a stretched texture across the band's height (glow strips,
/// the edge's soft light): `uv` y runs 0 at `y0` to 1 at `y1`.
pub fn tex_band(
    tex: TextureId,
    x0: f32,
    x1: f32,
    y0: f32,
    y1: f32,
    step: f32,
    col: impl Fn(f32) -> Color32,
) -> Mesh {
    let mut m = Mesh::with_texture(tex);
    if x1 <= x0 {
        return m;
    }
    let segs = (((x1 - x0) / step).ceil() as usize).max(1);
    for i in 0..=segs {
        let x = (x0 + step * i as f32).min(x1);
        let c = col(x);
        m.vertices.push(Vertex {
            pos: pos2(x, y0),
            uv: pos2(0.5, 0.0),
            color: c,
        });
        m.vertices.push(Vertex {
            pos: pos2(x, y1),
            uv: pos2(0.5, 1.0),
            color: c,
        });
        if i > 0 {
            let k = i as u32 * 2;
            m.indices
                .extend_from_slice(&[k - 2, k - 1, k, k - 1, k + 1, k]);
        }
    }
    m
}

/// A texture stretched over `rect`, tinted.
pub fn image(tex: TextureId, rect: Rect, tint: Color32) -> Mesh {
    let mut m = Mesh::with_texture(tex);
    m.add_rect_with_uv(rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), tint);
    m
}

/// CSS `box-shadow: 0 0 <blur>px <color>` around a disc of `radius`, as
/// Chrome (Skia) draws it: the disc blurred by a Gaussian of
/// σ = 0.57735 · blur / 2 + 0.5, as a mesh of rings.
pub fn disc_shadow(radius: f32, blur: f32, center: Pos2, color: Color32) -> Mesh {
    let sigma = (0.57735 * blur / 2.0 + 0.5).max(0.01);
    // the disc convolved with the Gaussian, at distance r from its centre
    // (numerically: a small disc's edge is well under the straight edge's ½)
    let a = |r: f32| {
        let n = 16;
        let span = radius + 3.0 * sigma;
        let step = 2.0 * span / n as f32;
        let mut sum = 0.0;
        let mut norm = 0.0;
        for j in 0..n {
            for i in 0..n {
                let dx = -span + (i as f32 + 0.5) * step;
                let dy = -span + (j as f32 + 0.5) * step;
                let g = (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp();
                norm += g;
                // sample point of the disc seen from distance r
                if (r + dx).powi(2) + dy * dy <= radius * radius {
                    sum += g;
                }
            }
        }
        sum / norm
    };
    let outer = radius + 3.0 * sigma;
    let rings = 12;
    let segs = 32;
    let mut m = Mesh::default();
    m.colored_vertex(center, alpha(color, a(0.0)));
    for k in 1..=rings {
        let r = outer * k as f32 / rings as f32;
        let c = alpha(color, a(r));
        for i in 0..segs {
            let t = std::f32::consts::TAU * i as f32 / segs as f32;
            m.colored_vertex(center + eframe::egui::vec2(t.cos(), t.sin()) * r, c);
        }
    }
    let ring = |k: u32, i: u32| {
        if k == 0 {
            0
        } else {
            1 + (k - 1) * segs + (i % segs)
        }
    };
    for k in 0..rings as u32 {
        for i in 0..segs {
            if k == 0 {
                m.add_triangle(0, ring(1, i), ring(1, i + 1));
            } else {
                m.add_triangle(ring(k, i), ring(k + 1, i), ring(k + 1, i + 1));
                m.add_triangle(ring(k, i), ring(k + 1, i + 1), ring(k, i + 1));
            }
        }
    }
    m
}

#[cfg(test)]
mod glow_tests {
    use super::*;

    #[test]
    fn the_glow_is_the_rules_sheets() {
        // across the line, as measured from the sheet (NOTES-3b)
        for (d, want) in [
            (0.0, 0.337),
            (2.0, 0.303),
            (4.0, 0.220),
            (6.0, 0.127),
            (8.0, 0.057),
            (10.0, 0.020),
        ] {
            let got = GLOW * glow_profile(d);
            assert!((got - want).abs() < 0.0015, "{d} px: {got} vs {want}");
        }
        assert!(GLOW * glow_profile(GLOW_HALF) < 0.005);
        assert!((glow_profile(0.0) - 0.747).abs() < 0.001);
        // its start: centred 4 px before the line's first pixel
        let x0 = 100.0;
        for (x, want) in [
            (x0, 0.874),
            (x0 - 4.0, 0.5),
            (x0 - 8.0, 0.126),
            (x0 - GLOW_CAP, 0.011),
        ] {
            let got = glow_cap(x, x0);
            assert!((got - want).abs() < 0.0015, "{x}: {got} vs {want}");
        }
        assert!(glow_cap(x0 + 4.0, x0) > 0.985);
    }
}
