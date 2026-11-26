//! Fonts and the type scale.
//!
//! egui draws one static file per weight (no variable fonts) and only
//! reads the legacy `kern` table, so the window embeds subset files (Latin,
//! Latin Extended-A, Cyrillic, punctuation, and Vietnamese drawn unkerned):
//! Geist 400, 500 and 600
//! with their GPOS kerning baked into a `kern` table and Chrome's
//! two-letter ligatures (ff, fi, fl, tt) written as kerning
//! (`scripts/bake-font-kerning.py`; unkerned, every line ran about 2 % wider
//! than the boards set it), and Geist Mono 400 for labels, keys and numbers
//! that change in place. A fifth, Geist 500 with the design system's
//! tabular figures applied to its character map (the components set
//! `font-feature-settings: "ss01", "tnum"`; the boards' Geist has no ss01, so
//! only tnum shows), sets the text inside its components:
//! buttons, the switch's label and the status pill ([`Type::component`]).
//! Licences: `licenses/OFL-Geist.txt` (in About).

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId};

const GEIST_400: &[u8] = include_bytes!("../../fonts/Geist-400-kern.ttf");
const GEIST_500: &[u8] = include_bytes!("../../fonts/Geist-500-kern.ttf");
const GEIST_500_DS: &[u8] = include_bytes!("../../fonts/Geist-500-ds-kern.ttf");
const GEIST_600: &[u8] = include_bytes!("../../fonts/Geist-600-kern.ttf");
const GEIST_MONO_400: &[u8] = include_bytes!("../../fonts/GeistMono-400.ttf");

const MEDIUM: &str = "geist-500";
const MEDIUM_DS: &str = "geist-500-ds";
const SEMIBOLD: &str = "geist-600";

/// The font definitions the window uses: Geist as the proportional family,
/// Geist Mono as the monospace one, and named families for 500, 500 with the
/// component features, and 600. Every family falls back to egui's own fonts
/// (Ubuntu Light: Greek, Vietnamese, symbols like ™; Noto Emoji), so a
/// microphone named in another script is not drawn as boxes; CJK stays
/// uncovered. Fallback glyphs are measured with Geist's missing-glyph
/// advance ([`line_width`]): centring is approximate on such text.
pub fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mono_fallback = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    for (name, bytes) in [
        ("geist-400", GEIST_400),
        (MEDIUM, GEIST_500),
        (MEDIUM_DS, GEIST_500_DS),
        (SEMIBOLD, GEIST_600),
        ("geist-mono-400", GEIST_MONO_400),
    ] {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    let family = |list: &[&str]| {
        let mut v: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        v.extend(fallback.iter().cloned());
        v
    };
    fonts.families.insert(
        FontFamily::Proportional,
        family(&["geist-400", "geist-mono-400"]),
    );
    let mut mono: Vec<String> = vec!["geist-mono-400".into(), "geist-400".into()];
    mono.extend(mono_fallback);
    fonts.families.insert(FontFamily::Monospace, mono);
    fonts.families.insert(
        FontFamily::Name(MEDIUM.into()),
        family(&[MEDIUM, "geist-400"]),
    );
    fonts.families.insert(
        FontFamily::Name(MEDIUM_DS.into()),
        family(&[MEDIUM_DS, "geist-400"]),
    );
    fonts.families.insert(
        FontFamily::Name(SEMIBOLD.into()),
        family(&[SEMIBOLD, "geist-400"]),
    );
    fonts
}

pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(definitions());
}

fn face(ty: Type) -> &'static ab_glyph::FontRef<'static> {
    use std::sync::OnceLock;
    static FACES: OnceLock<[ab_glyph::FontRef<'static>; 5]> = OnceLock::new();
    let faces = FACES.get_or_init(|| {
        [
            GEIST_400,
            GEIST_500,
            GEIST_600,
            GEIST_MONO_400,
            GEIST_500_DS,
        ]
        .map(|b| ab_glyph::FontRef::try_from_slice(b).expect("embedded font"))
    });
    &faces[match ty.weight {
        Weight::Regular => 0,
        Weight::Medium if ty.component => 4,
        Weight::Medium => 1,
        Weight::Semibold => 2,
        Weight::Mono => 3,
    }]
}

/// The exact width of one line's CSS box in `ty`, in points: advances,
/// kerning and tracking after every letter, the last one included, as the
/// browser sets it. (egui's galleys report the last glyph snapped to the
/// pixel, up to 1 px short, and leave out the last letter's tracking.)
pub fn line_width(text: &str, ty: Type) -> f32 {
    use ab_glyph::Font;
    let f = face(ty);
    let k = ty.size / f.units_per_em().unwrap_or(1000.0);
    let mut w = 0.0;
    let mut last = None;
    for c in text.chars() {
        let c = if ty.upper {
            c.to_uppercase().next().unwrap_or(c)
        } else {
            c
        };
        let id = f.glyph_id(c);
        if let Some(prev) = last {
            w += f.kern_unscaled(prev, id) * k;
        }
        w += f.h_advance_unscaled(id) * k + ty.tracking();
        last = Some(id);
    }
    w
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
    Mono,
}

/// One text style: size, weight, CSS line height and tracking. Every
/// style the app boards use is a constant below.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Type {
    pub size: f32,
    pub weight: Weight,
    /// CSS line-height in points: the height of one line box.
    pub line: f32,
    /// CSS letter-spacing in em.
    pub track_em: f32,
    /// Mono eyebrows are the one uppercase style.
    pub upper: bool,
    /// Text inside the design system's components (`.hm-btn`, `.hm-switch`,
    /// `.hm-pill`): tabular figures (CSS `font-feature-settings: "ss01",
    /// "tnum"`; Google Fonts' Geist has no ss01). Weight 500 only.
    pub component: bool,
}

impl Type {
    const fn new(size: f32, weight: Weight, line: f32) -> Type {
        Type {
            size,
            weight,
            line,
            track_em: 0.0,
            upper: false,
            component: false,
        }
    }
    const fn component(mut self) -> Type {
        self.component = true;
        self
    }
    const fn track(mut self, em: f32) -> Type {
        self.track_em = em;
        self
    }
    const fn upper(mut self) -> Type {
        self.upper = true;
        self
    }

    /// Page heading (`.hm-app-h`): 600 22/28, −0.015em.
    pub const H: Type = Type::new(22.0, Weight::Semibold, 28.0).track(-0.015);
    /// Card titles and About's name (`.hm-app-t3`): 600 18/24, −0.01em.
    pub const T3: Type = Type::new(18.0, Weight::Semibold, 24.0).track(-0.01);
    /// A heading's second line (`.hm-app-sub`): 14/20.
    pub const SUB: Type = Type::new(14.0, Weight::Regular, 20.0);
    /// The design system's body: 15/22.
    pub const BODY: Type = Type::new(15.0, Weight::Regular, 22.0);
    /// Emphasis in body size (Mic check's result words): 500 15/22.
    pub const BODY_STRONG: Type = Type::new(15.0, Weight::Medium, 22.0);
    /// Control labels (`.hm-app-label`, rail rows, selects): 500 14/20.
    pub const LABEL: Type = Type::new(14.0, Weight::Medium, 20.0);
    /// Captions (`.hm-app-cap`): 13/18.
    pub const CAP: Type = Type::new(13.0, Weight::Regular, 18.0);
    /// Inline links (`.hm-app-link`): 500 13/18.
    pub const LINK: Type = Type::new(13.0, Weight::Medium, 18.0);
    /// Mono eyebrows (`.hm-app-eyebrow`): 12/16, 0.10em, uppercase.
    pub const EYEBROW: Type = Type::new(12.0, Weight::Mono, 16.0).track(0.10).upper();
    /// Key chips, command chips, timeline labels: mono 12/16.
    pub const MONO: Type = Type::new(12.0, Weight::Mono, 16.0);
    /// Values next to controls (`readout`): mono 13/18.
    pub const READOUT: Type = Type::new(13.0, Weight::Mono, 18.0);
    /// Step numbers in first run: mono 11/14.
    pub const MONO_SMALL: Type = Type::new(11.0, Weight::Mono, 14.0);
    /// Buttons md (`.hm-btn`, weight 500 in the app): 14/20, −0.005em.
    pub const BUTTON: Type = Type::new(14.0, Weight::Medium, 20.0)
        .track(-0.005)
        .component();
    /// Buttons sm: 13/20, −0.005em.
    pub const BUTTON_SM: Type = Type::new(13.0, Weight::Medium, 20.0)
        .track(-0.005)
        .component();
    /// The switch's own label: 500 15/22.
    pub const SWITCH: Type = Type::new(15.0, Weight::Medium, 22.0).component();
    /// Segment and strength options: 500 13/18.
    pub const OPTION: Type = Type::new(13.0, Weight::Medium, 18.0);
    /// The status pill's word: 500 13 (line 13).
    pub const PILL: Type = Type::new(13.0, Weight::Medium, 13.0).component();
    /// Dropdown list rows: 500 13/18; their notes 12/16.
    pub const LIST: Type = Type::new(13.0, Weight::Medium, 18.0);
    pub const LIST_NOTE: Type = Type::new(12.0, Weight::Regular, 16.0);

    pub fn font(&self) -> FontId {
        let family = match self.weight {
            Weight::Regular => FontFamily::Proportional,
            Weight::Medium if self.component => FontFamily::Name(MEDIUM_DS.into()),
            Weight::Medium => FontFamily::Name(MEDIUM.into()),
            Weight::Semibold => FontFamily::Name(SEMIBOLD.into()),
            Weight::Mono => FontFamily::Monospace,
        };
        FontId::new(self.size, family)
    }

    /// Tracking in points.
    pub fn tracking(&self) -> f32 {
        self.track_em * self.size
    }

    /// Where the glyphs sit in the line box, as Chrome sets them: in CSS
    /// pixels (at any scale), ascent and descent rounded each (Geist: 1.005
    /// and 0.295 em), the leftover space halved and floored. Returns the
    /// offset of egui's row top from the line box's top in points: egui puts
    /// the baseline one ascent, rounded to device pixels, below the row top.
    pub fn half_leading_at(&self, ppp: f32) -> f32 {
        let a = (1.005 * self.size).round();
        let d = (0.295 * self.size).round();
        let half = ((self.line - a - d) / 2.0).floor();
        half + a - (1.005 * self.size * ppp).round() / ppp
    }

    /// [`Self::half_leading_at`] at 1x.
    pub fn half_leading(&self) -> f32 {
        self.half_leading_at(1.0)
    }

    /// The baseline below the line box's top, in points (the same at every
    /// scale).
    pub fn baseline_at(&self, ppp: f32) -> f32 {
        self.half_leading_at(ppp) + (1.005 * self.size * ppp).round() / ppp
    }

    pub fn with_line(mut self, line: f32) -> Type {
        self.line = line;
        self
    }

    pub fn with_size(mut self, size: f32) -> Type {
        self.size = size;
        self
    }

    pub fn with_track(mut self, em: f32) -> Type {
        self.track_em = em;
        self
    }

    /// The same style in weight 500 (`<strong>` inside captions).
    pub fn medium(mut self) -> Type {
        self.weight = Weight::Medium;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baselines_land_where_chrome_puts_them() {
        // measured in Chrome (Geist, grayscale text, 1x): size/line → baseline
        for (size, line, base) in [
            (11.0, 14.0, 11.0),
            (12.0, 16.0, 12.0),
            (13.0, 18.0, 13.0),
            (13.0, 20.0, 14.0),
            (14.0, 20.0, 15.0),
            (15.0, 22.0, 16.0),
            (18.0, 24.0, 18.0),
            (22.0, 28.0, 22.0),
            (13.0, 13.0, 11.0),
            (13.0, 30.0, 19.0),
            (14.0, 40.0, 25.0),
            (12.0, 26.0, 17.0),
        ] {
            let t = Type::new(size, Weight::Regular, line);
            assert_eq!(t.baseline_at(1.0), base, "{size}/{line}");
            // Chrome lays out in CSS pixels: 2x puts the baseline at twice that
            assert_eq!(t.baseline_at(2.0), base, "{size}/{line} at 2x");
        }
    }

    #[test]
    fn other_scripts_fall_back_instead_of_boxes() {
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            for ty in [Type::LABEL, Type::CAP, Type::BUTTON, Type::H, Type::MONO] {
                for s in [
                    "Ενσωματωμένος ήχος",
                    "Âm thanh tích hợp",
                    "Jabra Evolve2 65™",
                    "🎧",
                ] {
                    let covered = ctx.fonts_mut(|f| f.has_glyphs(&ty.font(), s));
                    assert!(covered, "{s:?} in {ty:?}");
                }
            }
        });
    }

    #[test]
    fn the_embedded_fonts_parse() {
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            let w = ctx.fonts_mut(|f| {
                f.layout_no_wrap("Ready.".into(), Type::H.font(), egui::Color32::WHITE)
                    .size()
                    .x
            });
            assert!(w > 40.0 && w < 90.0, "{w}");
        });
    }
}

#[cfg(test)]
mod widths {
    use super::*;

    /// The boards' text widths, measured in Chrome with the same fonts
    /// (kerning on): egui must set the same lines within a fraction of a
    /// pixel, or wrapping and centring drift from the boards.
    #[test]
    fn lines_are_as_wide_as_the_boards_set_them() {
        let ctx = egui::Context::default();
        install(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            for (s, t, chrome) in [
                ("Removing background noise.", Type::H, 293.92),
                ("Microphone", Type::LABEL, 77.17),
                ("Live", Type::LABEL, 26.53),
                ("Noise suppression", Type::SWITCH, 129.05),
                ("Gentle", Type::OPTION, 40.34),
                ("Quality", Type::OPTION, 43.33),
                // inside a design-system button: tabular figures
                ("Replay the last 10 seconds", Type::BUTTON, 175.27),
                // Chrome's ligatures (ff, fi, fl, tt), baked in as kerning
                (
                    "Takes effect the next time HushMic starts.",
                    Type::CAP,
                    248.98,
                ),
                ("Settings", Type::LABEL, 54.67),
                ("Notifications", Type::LABEL, 83.19),
                ("Different", Type::CAP, 50.92),
                ("Input", Type::EYEBROW, 42.02),
            ] {
                let w = crate::ui::text::width(ctx, s, t);
                assert!(
                    (w - chrome).abs() < 0.2,
                    "{s:?}: egui {w:.2}, board {chrome:.2}"
                );
            }
        });
    }
}
