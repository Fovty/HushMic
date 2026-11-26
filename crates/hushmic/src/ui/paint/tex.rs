//! The light textures, made once when the window opens (AppFx `textures()`):
//! white with alpha from a falloff function and ±0.5 LSB triangular dither,
//! so long faint ramps do not band near black. They are drawn stretched,
//! tinted by vertex colour and, in dark, additive.

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureId, TextureOptions};

pub struct Textures {
    /// Across a lit line: [`super::glow_profile`], centre alpha 0.747
    /// (drawn 26 px tall, 2 rows a pixel).
    pub glow: TextureHandle,
    /// The beam: a 1.5 px core in a soft halo, brighter towards its head.
    pub beam: TextureHandle,
    /// Where the beam lands: a bloom above the line only.
    pub splash: TextureHandle,
    /// The playhead's soft light, 12 px wide.
    pub head: TextureHandle,
}

impl Textures {
    pub fn upload(ctx: &egui::Context) -> Textures {
        Textures {
            glow: make(ctx, "hm-glow", 4, 52, |_, y, _, h| {
                super::glow_profile((y + 0.5 - h / 2.0) * 2.0 * super::GLOW_HALF / h)
            }),
            beam: make(ctx, "hm-beam", 48, 256, |x, y, w, h| {
                let u = x + 0.5 - w / 2.0;
                let v = (y + 0.5) / h;
                ((gauss(u, 1.1) + 0.28 * gauss(u, 7.0)) * v.powf(1.6)).min(1.0)
            }),
            splash: make(ctx, "hm-splash", 128, 48, |x, y, w, h| {
                let u = (x + 0.5 - w / 2.0) / (w / 2.0);
                let v = (h - (y + 0.5)) / h;
                0.9 * gauss(u, 0.32) * gauss(v, 0.38)
            }),
            head: make(ctx, "hm-head", 24, 4, |x, _, w, _| {
                0.22 * gauss(x + 0.5 - w / 2.0, 3.2)
            }),
        }
    }

    pub fn ids(&self) -> [TextureId; 4] {
        [
            self.glow.id(),
            self.beam.id(),
            self.splash.id(),
            self.head.id(),
        ]
    }
}

fn gauss(u: f32, s: f32) -> f32 {
    (-(u * u) / (2.0 * s * s)).exp()
}

fn make(
    ctx: &egui::Context,
    name: &str,
    w: usize,
    h: usize,
    f: impl Fn(f32, f32, f32, f32) -> f32,
) -> TextureHandle {
    // The same Park–Miller generator as AppFx, so the dither matches.
    let mut seed: u64 = 1_234_567;
    let mut rnd = || {
        seed = (seed * 16807) % 2_147_483_647;
        (seed as f32 - 1.0) / 2_147_483_646.0
    };
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let a = f(x as f32, y as f32, w as f32, h as f32);
            let v = (a * 255.0 + (rnd() - rnd()) * 0.5)
                .round()
                .clamp(0.0, 255.0) as u8;
            // white, premultiplied
            px.push(Color32::from_rgba_premultiplied(v, v, v, v));
        }
    }
    ctx.load_texture(name, ColorImage::new([w, h], px), TextureOptions::LINEAR)
}
