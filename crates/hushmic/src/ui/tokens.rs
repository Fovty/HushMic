//! Design tokens as egui values, two palettes.
//!
//! Dark is the brand theme: a near-black ground with a cool cast (not pure
//! black: a card has to read as a surface on it), cards a step lighter,
//! the cyan accent as the design system has it.
//!
//! Light is a grey scheme of its own, not dark inverted: a neutral mid
//! grey ground (no blue cast: that read as clinical), cards clearly
//! lighter and lying a little above it (a soft shadow), buttons filled a
//! step lighter still, nothing white, the accent darkened until it
//! carries text. The pictures of sound stay dark in it
//! (`ui::paint::spectro`).
//!
//! The website keeps the design system's pure black; these are the app's.

use eframe::egui::{self, Color32};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub fn is_dark(self) -> bool {
        self == Theme::Dark
    }
}

/// Every colour token, resolved for one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub theme: Theme,
    pub bg: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub surface_hover: Color32,
    pub line: Color32,
    pub line_strong: Color32,
    pub ink: Color32,
    pub ink_muted: Color32,
    pub ink_subtle: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub accent_soft: Color32,
    pub accent_line: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub scrim: Color32,
    pub frame_light: Color32,
    /// What the light arrives as before it cools to the accent: white in
    /// dark, ink in light (AppLive's `arrive`).
    pub light: Color32,
}

pub const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// `rgba(r, g, b, a)` as CSS writes it (unmultiplied, alpha 0..1).
pub fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, (a * 255.0).round() as u8)
}

impl Palette {
    pub fn of(theme: Theme) -> Palette {
        match theme {
            Theme::Dark => Palette {
                theme,
                bg: hex(0x0a0c0d),
                surface: hex(0x121516),
                surface_raised: hex(0x1b1f20),
                surface_hover: hex(0x23282a),
                line: hex(0x262b2d),
                line_strong: hex(0x687071),
                ink: hex(0xf1f4f4),
                ink_muted: hex(0xa2aaab),
                ink_subtle: hex(0x828a8b),
                accent: hex(0x00f0f8),
                on_accent: hex(0x000000),
                accent_soft: rgba(0, 240, 248, 0.12),
                accent_line: rgba(0, 240, 248, 0.40),
                warn: hex(0xffb224),
                danger: hex(0xff5a4a),
                scrim: rgba(0, 0, 0, 0.80),
                frame_light: rgba(255, 255, 255, 0.10),
                light: hex(0xffffff),
            },
            Theme::Light => Palette {
                theme,
                bg: hex(0xbdbdbb),
                surface: hex(0xdcdcda),
                surface_raised: hex(0xececea),
                surface_hover: hex(0xcbcbc9),
                line: hex(0xb4b4b1),
                line_strong: hex(0x626260),
                ink: hex(0x141413),
                ink_muted: hex(0x3a3a38),
                ink_subtle: hex(0x4a4a48),
                accent: hex(0x00545a),
                on_accent: hex(0xf4f4f3),
                accent_soft: rgba(0, 84, 90, 0.12),
                accent_line: rgba(0, 84, 90, 0.45),
                warn: hex(0x5f3c00),
                danger: hex(0x8f170e),
                scrim: rgba(20, 20, 19, 0.55),
                frame_light: rgba(0, 0, 0, 0.05),
                light: hex(0x141413),
            },
        }
    }

    pub fn is_dark(&self) -> bool {
        self.theme.is_dark()
    }

    /// The focus ring is the accent in both themes.
    pub fn focus_ring(&self) -> Color32 {
        self.accent
    }

    /// The palette a picture of sound is painted in (the lanes on Live
    /// and Files, and what is written or sits in them): the dark one in
    /// both themes. A spectrogram needs a dark ground to show a quiet
    /// floor beside a loud voice, and in the grey scheme it stands as a
    /// screen stands in a light room. Lanes with nothing to show yet are
    /// not a picture: `ui::paint::spectro::palette` decides.
    pub fn picture(&self) -> Palette {
        Palette::of(Theme::Dark)
    }

    /// This palette `t` of the way to `to`, every colour (a surface that
    /// changes palettes eases through it).
    pub fn blend(&self, to: &Palette, t: f32) -> Palette {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return *self;
        }
        if t >= 1.0 {
            return *to;
        }
        let m = |a: Color32, b: Color32| {
            let (a, b) = (a.to_srgba_unmultiplied(), b.to_srgba_unmultiplied());
            let c = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8;
            Color32::from_rgba_unmultiplied(c(0), c(1), c(2), c(3))
        };
        Palette {
            theme: if t < 0.5 { self.theme } else { to.theme },
            bg: m(self.bg, to.bg),
            surface: m(self.surface, to.surface),
            surface_raised: m(self.surface_raised, to.surface_raised),
            surface_hover: m(self.surface_hover, to.surface_hover),
            line: m(self.line, to.line),
            line_strong: m(self.line_strong, to.line_strong),
            ink: m(self.ink, to.ink),
            ink_muted: m(self.ink_muted, to.ink_muted),
            ink_subtle: m(self.ink_subtle, to.ink_subtle),
            accent: m(self.accent, to.accent),
            on_accent: m(self.on_accent, to.on_accent),
            accent_soft: m(self.accent_soft, to.accent_soft),
            accent_line: m(self.accent_line, to.accent_line),
            warn: m(self.warn, to.warn),
            danger: m(self.danger, to.danger),
            scrim: m(self.scrim, to.scrim),
            frame_light: m(self.frame_light, to.frame_light),
            light: m(self.light, to.light),
        }
    }

    /// Every card's fill (UX panel D1: one card style on every page, a
    /// filled surface in a `line` hairline; Live's and Files' figures were
    /// outlined on the page's black, Settings' groups filled).
    pub fn card_fill(&self) -> Color32 {
        self.surface
    }

    /// What lies in a card, a step below it (a segmented control's
    /// track): the card's own surface in dark, the page's ground in light,
    /// where a card is lighter than the page.
    pub fn inset(&self) -> Color32 {
        if self.is_dark() {
            self.surface
        } else {
            self.bg
        }
    }

    /// An outlined button's fill at rest: none in dark; in light a step
    /// lighter than a card, so it reads as a button and not as a wire on
    /// the grey.
    pub fn button_fill(&self) -> Color32 {
        if self.is_dark() {
            Color32::TRANSPARENT
        } else {
            self.surface_raised
        }
    }

    /// An outlined button held down: raised in dark; pressed in (darker
    /// than its fill at rest) in light.
    pub fn button_held(&self) -> Color32 {
        if self.is_dark() {
            self.surface_raised
        } else {
            self.surface_hover
        }
    }

    /// A field's fill (a select at rest): the page's ground in dark, the
    /// card's surface in light.
    pub fn field_fill(&self) -> Color32 {
        if self.is_dark() {
            self.bg
        } else {
            self.surface
        }
    }

    /// A card's shadow: none in dark; in light a card lies a little above
    /// the grey ground.
    pub fn card_shadow(&self) -> Option<egui::Shadow> {
        (!self.is_dark()).then_some(egui::Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: rgba(20, 20, 19, 0.10),
        })
    }

    /// A switch that is off: (track fill, track edge, knob). Outlined
    /// with a grey knob in dark; a filled grey track with a light knob in
    /// light (the track is 3:1 on a card).
    pub fn switch_off(&self) -> (Color32, Color32, Color32) {
        if self.is_dark() {
            (Color32::TRANSPARENT, self.line_strong, self.ink_muted)
        } else {
            (hex(0x70706e), hex(0x70706e), hex(0xececea))
        }
    }

    /// The raw microphone's waveform: ink-muted at this alpha; 3:1 on the
    /// page in light (UX panel B17).
    pub fn raw_alpha(&self) -> f32 {
        if self.is_dark() {
            0.42
        } else {
            0.66
        }
    }

    /// The edge a highlighted row (pointer, keyboard highlight) gets beside
    /// its fill: none in dark, line-strong in light, where the fill alone
    /// is too close to the ground (UX panel B17; the edge is 3:1).
    pub fn highlight_edge(&self) -> Option<Color32> {
        (!self.is_dark()).then_some(self.line_strong)
    }

    /// The selection, one model in both themes (UX panel round 2, A18): a
    /// filled pill in a hairline, as the current page in the rail, a
    /// selected list row or the chosen option of a list. Its fill: the
    /// raised surface in dark; in light the ink at 13 %, so it reads on
    /// the page and on a card alike.
    pub fn selected_fill(&self) -> Color32 {
        if self.is_dark() {
            self.surface_raised
        } else {
            rgba(20, 20, 19, 0.13)
        }
    }

    /// The selection's hairline (see [`Self::selected_fill`]): `line` in
    /// dark, the ink at 22 % in light. Never the accent: in light a teal
    /// edge looked like the focus ring (A18, A32).
    pub fn selected_line(&self) -> Color32 {
        if self.is_dark() {
            self.line
        } else {
            rgba(20, 20, 19, 0.22)
        }
    }

    /// The edge of a selected row: `dark` in dark, the selection's
    /// hairline in light. Going: draw a selection with
    /// `widgets::selected`.
    pub fn selected_edge(&self, dark: Color32) -> Color32 {
        if self.is_dark() {
            dark
        } else {
            self.selected_line()
        }
    }
}

/// Spacing (`space-1` … `space-10`), radii and durations.
pub mod space {
    pub const S1: f32 = 4.0;
    pub const S2: f32 = 8.0;
    pub const S3: f32 = 12.0;
    pub const S4: f32 = 16.0;
    pub const S5: f32 = 24.0;
    pub const S6: f32 = 32.0;
    pub const S7: f32 = 48.0;
    pub const S8: f32 = 64.0;
}

pub mod radius {
    /// Meter cells, keys, inline code.
    pub const XS: f32 = 4.0;
    /// Buttons, inputs, segmented control.
    pub const SM: f32 = 8.0;
    /// Panels and cards in the app.
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 20.0;
}

pub mod duration {
    /// Hover, press, focus.
    pub const FAST: f32 = 0.120;
    /// Toggles, segment changes.
    pub const BASE: f32 = 0.220;
    /// Panels entering.
    pub const SLOW: f32 = 0.480;
    pub const SETTLE: f32 = 0.900;
}

/// The window sizes the boards are drawn at (logical points, content only:
/// the desktop draws the title bar).
pub mod window {
    pub const DEFAULT: [f32; 2] = [960.0, 640.0];
    pub const MINIMUM: [f32; 2] = [720.0, 520.0];
    /// Below this content width the window uses the compact layout (icon
    /// rail, stacked chain): the boards' "Minimum 720 × 520".
    pub const COMPACT_BELOW: f32 = 840.0;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// WCAG contrast of two opaque colours.
    pub(crate) fn contrast(a: Color32, b: Color32) -> f32 {
        let l = |c: Color32| {
            let ch = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
        };
        let (x, y) = (l(a), l(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// `c` at `a` over the opaque `bg`, blended as egui does (gamma space).
    fn over(c: Color32, a: f32, bg: Color32) -> Color32 {
        let m = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
        Color32::from_rgb(m(c.r(), bg.r()), m(c.g(), bg.g()), m(c.b(), bg.b()))
    }

    #[test]
    fn light_states_and_the_raw_wave_reach_3_to_1() {
        let l = Palette::of(Theme::Light);
        // the raw waveform on the page
        let raw = over(l.ink_muted, l.raw_alpha(), l.bg);
        assert!(contrast(raw, l.bg) >= 3.0, "{raw:?}");
        // a highlight's edge, on the page and on cards
        for bg in [l.bg, l.surface] {
            assert!(contrast(l.highlight_edge().unwrap(), bg) >= 3.0);
        }
        // the selection is a filled pill in both themes (A18): its fill
        // shows on the page and on a card, darker than the hover fill, and
        // nothing of it is the accent
        for bg in [l.bg, l.surface] {
            let fill = bg.blend(l.selected_fill());
            assert!(
                fill.r() < bg.r() && fill.r() < l.surface_hover.r(),
                "{fill:?}"
            );
        }
        assert_ne!(l.selected_line(), l.accent);
        assert_ne!(l.highlight_edge(), Some(l.selected_line()));
        // dark: the selection is the raised surface, no edge beside it
        let d = Palette::of(Theme::Dark);
        assert_eq!(d.raw_alpha(), 0.42);
        assert_eq!(d.highlight_edge(), None);
        assert_eq!(
            (d.selected_fill(), d.selected_line()),
            (d.surface_raised, d.line)
        );
        assert_eq!(d.selected_edge(d.line), d.line);
    }

    #[test]
    fn soft_accents_are_the_accent_thinned() {
        for theme in [Theme::Dark, Theme::Light] {
            let p = Palette::of(theme);
            for soft in [p.accent_soft, p.accent_line] {
                // premultiplied, so a channel may come back one or two off
                let c = soft.to_srgba_unmultiplied();
                for (got, want) in c.iter().zip([p.accent.r(), p.accent.g(), p.accent.b()]) {
                    assert!(got.abs_diff(want) <= 4, "{theme:?}: {c:?}");
                }
            }
            assert_eq!(p.accent_soft.a(), 31, "{theme:?}: 12 %");
            assert!(p.accent_line.a() > p.accent_soft.a(), "{theme:?}");
        }
    }

    /// Both palettes by number: words reach 4.5:1 and what marks a control
    /// reaches 3:1, on the page and on a card.
    #[test]
    fn both_palettes_carry_their_words_and_their_controls() {
        for theme in [Theme::Dark, Theme::Light] {
            let p = Palette::of(theme);
            // also on a selected row of a card's list
            let picked = |bg: Color32| bg.blend(p.selected_fill());
            for bg in [p.bg, p.surface, p.surface_raised, picked(p.surface)] {
                for (name, ink) in [
                    ("ink", p.ink),
                    ("ink_muted", p.ink_muted),
                    ("ink_subtle", p.ink_subtle),
                    ("accent", p.accent),
                    ("warn", p.warn),
                    ("danger", p.danger),
                ] {
                    assert!(contrast(ink, bg) >= 4.5, "{theme:?} {name} on {bg:?}");
                }
                assert!(contrast(p.line_strong, bg) >= 3.0, "{theme:?} {bg:?}");
            }
            // the rail's current page is a selection on the page's ground:
            // a label and its glyph
            for ink in [p.ink, p.ink_muted] {
                assert!(contrast(ink, picked(p.bg)) >= 4.5, "{theme:?}");
            }
            // a filled accent carries its own ink, and a primary button's
            // fill (the ink) its words (the page's ground)
            assert!(contrast(p.on_accent, p.accent) >= 4.5, "{theme:?}");
            assert!(contrast(p.bg, p.ink) >= 4.5, "{theme:?}");
            // a card is a surface on the page, lighter than it, in both
            assert!(contrast(p.surface, p.bg) > 1.04, "{theme:?}");
            let lum = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
            assert!(lum(p.surface) > lum(p.bg), "{theme:?}");
            assert!(lum(p.surface_raised) > lum(p.surface), "{theme:?}");
            // neither end is pure
            for c in [p.bg, p.surface, p.surface_raised, p.ink] {
                assert!(c != hex(0x000000) && c != hex(0xffffff), "{theme:?}");
            }
        }
        // light: a switch that is off shows on a card
        let l = Palette::of(Theme::Light);
        let (track, _, knob) = l.switch_off();
        assert!(contrast(track, l.surface) >= 3.0);
        assert!(contrast(knob, track) >= 3.0);
        assert_eq!(Palette::of(Theme::Dark).card_shadow(), None);
        assert!(l.card_shadow().is_some());
    }
}
