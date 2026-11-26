//! The HushMic window's design system in egui: tokens, fonts, painted icons,
//! widgets, the Live / take painters and the light layer.
//!
//! Pages call [`install`] once, [`begin_frame`] at the top of every
//! `update`, then draw with the widgets in [`widgets`] and the painters in
//! [`paint`]. The kit's state (theme, reduced motion, software renderer,
//! textures) lives in the egui context, so every widget finds it with
//! [`kit`].

pub mod fonts;
pub mod gallery;
pub mod icons;
pub mod licenses;
mod licenses_crates;
pub mod motion;
pub mod paint;
pub mod replica;
pub mod shot;
pub mod text;
pub mod tokens;
pub mod widgets;

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Id, Stroke};

pub use fonts::{Type, Weight};
pub use icons::Icon;
pub use tokens::{Palette, Theme};

/// The kit's per-window state.
#[derive(Clone)]
pub struct Kit {
    pub pal: Palette,
    /// The desktop asks for less motion: every change is instant, the beam
    /// never falls, live columns keep updating without smoothing.
    pub reduced_motion: bool,
    /// OpenGL is emulated on the CPU (llvmpipe, softpipe, SwiftShader):
    /// live views at 10 Hz, moments capped near 30 fps, no glow strips and
    /// no soft light.
    pub software: bool,
    /// One display frame in seconds (60 Hz unless the runner knows better).
    pub frame_period: f32,
    /// The last focus change came from the keyboard, so focus rings show
    /// (CSS `:focus-visible`).
    pub focus_visible: bool,
    pub(crate) tex: Option<Arc<paint::tex::Textures>>,
}

impl Default for Kit {
    fn default() -> Kit {
        Kit {
            pal: Palette::of(Theme::Dark),
            reduced_motion: false,
            software: false,
            frame_period: 1.0 / 60.0,
            focus_visible: false,
            tex: None,
        }
    }
}

impl Kit {
    pub fn dark(&self) -> bool {
        self.pal.is_dark()
    }

    /// The light textures (glow, beam, splash, playhead), uploaded at
    /// [`install`].
    pub fn tex(&self) -> &paint::tex::Textures {
        self.tex
            .as_deref()
            .expect("ui::install() uploads the textures")
    }
}

fn kit_id() -> Id {
    Id::new("hm-kit")
}

/// The kit's state for this window.
pub fn kit(ctx: &egui::Context) -> Kit {
    ctx.data(|d| d.get_temp::<Kit>(kit_id()))
        .unwrap_or_default()
}

fn update_kit(ctx: &egui::Context, f: impl FnOnce(&mut Kit)) {
    let mut k = kit(ctx);
    f(&mut k);
    ctx.data_mut(|d| d.insert_temp(kit_id(), k));
}

/// Fonts, textures, style and theme. Call once when the window opens (before
/// the first frame); the textures are uploaded here so the first frame of a
/// moment needs no GPU work.
pub fn install(ctx: &egui::Context, theme: Theme) {
    fonts::install(ctx);
    let tex = Arc::new(paint::tex::Textures::upload(ctx));
    update_kit(ctx, |k| k.tex = Some(tex));
    set_theme(ctx, theme);
}

pub fn set_theme(ctx: &egui::Context, theme: Theme) {
    let pal = Palette::of(theme);
    update_kit(ctx, |k| k.pal = pal);
    let egui_theme = if theme.is_dark() {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(egui_theme);
    let mut s = style(&pal);
    motion_style(&mut s, kit(ctx).reduced_motion);
    ctx.set_style_of(egui_theme, s);
}

/// The desktop asks for less motion (or no longer does). Applies to both
/// themes' styles, so a later [`set_theme`] keeps it.
pub fn set_reduced_motion(ctx: &egui::Context, on: bool) {
    update_kit(ctx, |k| k.reduced_motion = on);
    ctx.all_styles_mut(|s| motion_style(s, on));
}

/// egui's own animations (popup fades, scroll-to, collapsing) under the
/// reduced-motion switch.
fn motion_style(s: &mut egui::Style, reduced: bool) {
    if reduced {
        s.animation_time = 0.0;
        s.scroll_animation = egui::style::ScrollAnimation::none();
    } else {
        s.animation_time = tokens::duration::FAST;
        s.scroll_animation = egui::style::ScrollAnimation::default();
    }
}

pub fn set_software(ctx: &egui::Context, on: bool) {
    update_kit(ctx, |k| k.software = on);
}

/// The display's frame period for the software-GL cap ([`motion::moment_repaint`]).
/// Known limit: eframe does not hand winit's refresh rate on, so nothing
/// sets it yet and the cap assumes 60 Hz (on a 144 Hz screen a moment runs
/// near 36 fps on uneven vblanks). GPU renderers are not affected.
pub fn set_frame_period(ctx: &egui::Context, seconds: f32) {
    update_kit(ctx, |k| k.frame_period = seconds);
}

/// Whether the OpenGL renderer is a software one. The runner calls this
/// once with `frame.gl()`; `HUSHMIC_UI_SOFTWARE=0|1` overrides it.
pub fn renderer_is_software(gl: Option<&eframe::glow::Context>) -> bool {
    if let Ok(v) = std::env::var("HUSHMIC_UI_SOFTWARE") {
        return v == "1";
    }
    use eframe::glow::HasContext;
    let Some(gl) = gl else { return false };
    // SAFETY: a plain string query on the current context.
    let name = unsafe { gl.get_parameter_string(eframe::glow::RENDERER) }.to_lowercase();
    ["llvmpipe", "softpipe", "swiftshader", "software rasterizer"]
        .iter()
        .any(|s| name.contains(s))
}

/// Show or hide focus rings (keyboard use shows them; [`begin_frame`] sets
/// it from Tab, the arrows, Home and End, and clears it on a pointer press).
pub fn set_focus_visible(ctx: &egui::Context, on: bool) {
    update_kit(ctx, |k| k.focus_visible = on);
}

/// Give `id` keyboard focus with its ring showing: for a control a
/// keyboard shortcut brought up (Ctrl+Q's sheet focusing Cancel). A
/// pointer-opened sheet requests focus the plain way and stays ringless.
pub fn keyboard_focus(ctx: &egui::Context, id: egui::Id) {
    ctx.memory_mut(|m| m.request_focus(id));
    set_focus_visible(ctx, true);
}

/// Call at the end of every frame: the arrows never move keyboard focus
/// from one control to another across the window (UX panel B21). egui
/// moves it to the nearest control in the arrow's direction, out of any
/// group: Down from the last tab opened More, Down from Continue landed
/// on Skip. Groups and lists move their own focus with the arrows; Tab
/// moves between controls.
pub fn end_frame(ctx: &egui::Context) {
    ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
}

/// A held Enter or Space after its first press: the keyboard's auto-repeat.
/// egui counts these as presses (a focused control clicks on each one), so
/// [`begin_frame`] drops them before anything reads the keys (UX panel A31:
/// a held Space flipped Mute 31 times in 2 s, a held Enter on Save opened 25
/// file managers). The arrows, Tab and the rest keep repeating.
pub fn activation_repeat(e: &egui::Event) -> bool {
    matches!(
        e,
        egui::Event::Key {
            key: egui::Key::Enter | egui::Key::Space,
            pressed: true,
            repeat: true,
            ..
        }
    )
}

/// Call at the top of every frame: one activation per physical press of
/// Enter or Space, for every control ([`activation_repeat`]); tracks
/// keyboard versus pointer focus.
pub fn begin_frame(ctx: &egui::Context) {
    ctx.input_mut(|i| i.events.retain(|e| !activation_repeat(e)));
    let (keyboard, pointer) = ctx.input(|i| {
        let mut keyboard = false;
        let mut pointer = false;
        for e in &i.raw.events {
            match e {
                egui::Event::Key {
                    key, pressed: true, ..
                } => {
                    use egui::Key::*;
                    if matches!(
                        key,
                        Tab | ArrowUp | ArrowDown | ArrowLeft | ArrowRight | Home | End
                    ) {
                        keyboard = true;
                    }
                }
                egui::Event::PointerButton { pressed: true, .. } => pointer = true,
                _ => {}
            }
        }
        (keyboard, pointer)
    });
    if keyboard || pointer {
        update_kit(ctx, |k| k.focus_visible = keyboard && !pointer);
    }
    // the window's name for screen readers: egui leaves its root unnamed,
    // and winit cannot read the title back on X11 or Wayland
    let title = ctx
        .input(|i| i.viewport().title.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "HushMic".to_owned());
    ctx.accesskit_node_builder(egui::accesskit_root_id(), |n| n.set_label(title));
}

/// egui's own visuals in the kit's colours, for anything drawn by egui
/// itself (scroll bars, text cursors, tooltips). The kit's widgets paint
/// themselves from [`Palette`].
pub fn style(pal: &Palette) -> egui::Style {
    let mut v = if pal.is_dark() {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = pal.bg;
    v.window_fill = pal.surface_raised;
    v.faint_bg_color = pal.surface;
    v.extreme_bg_color = pal.surface_raised;
    v.code_bg_color = pal.surface_raised;
    v.window_stroke = Stroke::new(1.0_f32, pal.line);
    v.window_corner_radius = CornerRadius::same(tokens::radius::MD as u8);
    v.menu_corner_radius = CornerRadius::same(tokens::radius::MD as u8);
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = shadow_pop(pal);
    v.override_text_color = Some(pal.ink);
    v.hyperlink_color = pal.accent;
    v.selection.bg_fill = pal.accent_soft;
    v.selection.stroke = Stroke::new(1.0_f32, pal.accent);
    v.text_cursor.stroke = Stroke::new(2.0_f32, pal.ink);
    v.warn_fg_color = pal.warn;
    v.error_fg_color = pal.danger;
    for (w, fill) in [
        (&mut v.widgets.noninteractive, pal.bg),
        (&mut v.widgets.inactive, pal.surface),
        (&mut v.widgets.hovered, pal.surface_hover),
        (&mut v.widgets.active, pal.surface_hover),
        (&mut v.widgets.open, pal.surface_raised),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0_f32, pal.line);
        w.fg_stroke = Stroke::new(1.0_f32, pal.ink);
        w.corner_radius = CornerRadius::same(tokens::radius::SM as u8);
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, pal.ink_muted);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, pal.line_strong);
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    // Text weight as the browser sets it (Skia's grayscale text): measured
    // against the boards, light text on black matches at coverage^0.85,
    // dark text on white at plain coverage (egui's own dark default draws
    // text visibly bolder).
    v.text_alpha_from_coverage = if pal.is_dark() {
        egui::epaint::AlphaFromCoverage::Gamma(0.85)
    } else {
        egui::epaint::AlphaFromCoverage::Linear
    };

    let mut s = egui::Style {
        visuals: v,
        ..Default::default()
    };
    // The boards space everything explicitly; nothing is added between items.
    s.spacing.item_spacing = egui::vec2(0.0, 0.0);
    s.spacing.button_padding = egui::vec2(0.0, 0.0);
    s.spacing.window_margin = egui::Margin::ZERO;
    s.spacing.menu_margin = egui::Margin::same(6);
    s.spacing.interact_size = egui::vec2(32.0, 32.0);
    // Round 3: whatever scrolls shows its bar at rest (a thin handle in the
    // gutter), widening under the pointer; it floats in the page's side
    // padding, never over the column.
    s.spacing.scroll = egui::style::ScrollStyle {
        floating_width: 4.0,
        dormant_background_opacity: 0.0,
        dormant_handle_opacity: 0.5,
        active_handle_opacity: 0.7,
        ..egui::style::ScrollStyle::floating()
    };
    s.animation_time = tokens::duration::FAST;
    s.text_styles = [
        (egui::TextStyle::Small, Type::CAP.font()),
        (egui::TextStyle::Body, Type::SUB.font()),
        (egui::TextStyle::Button, Type::BUTTON.font()),
        (egui::TextStyle::Heading, Type::H.font()),
        (egui::TextStyle::Monospace, Type::MONO.font()),
    ]
    .into();
    s.url_in_tooltip = false;
    s
}

/// `shadow-pop` for menus and popovers: a 1 px ring in `line` (drawn by the
/// menu itself) and a soft drop.
pub fn shadow_pop(pal: &Palette) -> egui::Shadow {
    if pal.is_dark() {
        egui::Shadow {
            offset: [0, 16],
            blur: 48,
            spread: 0,
            color: Color32::from_black_alpha(153),
        }
    } else {
        egui::Shadow {
            offset: [0, 12],
            blur: 32,
            spread: 0,
            color: Color32::from_black_alpha(20),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortcut_can_focus_with_the_ring() {
        let ctx = egui::Context::default();
        install(&ctx, Theme::Dark);
        // one frame with a Cancel button; returns its id and whether its ring shows
        let frame = |f: &dyn Fn(&egui::Context, egui::Id)| {
            let mut out = (egui::Id::NULL, false);
            let _ = ctx.run(Default::default(), |ctx| {
                begin_frame(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    let r = ui.add(widgets::Button::secondary("Cancel"));
                    f(ctx, r.id);
                    out = (r.id, widgets::shows_focus(&kit(ctx), &r));
                });
            });
            out
        };
        frame(&|_, _| {});
        frame(&|ctx, id| keyboard_focus(ctx, id));
        let (id, ring) = frame(&|_, _| {});
        assert!(ctx.memory(|m| m.has_focus(id)));
        assert!(ring, "focused from the keyboard: the ring shows");
        set_focus_visible(&ctx, false);
        assert!(!frame(&|_, _| {}).1);
    }

    #[test]
    fn only_the_repeats_of_enter_and_space_are_dropped() {
        let key = |key, pressed, repeat| egui::Event::Key {
            key,
            pressed,
            repeat,
            modifiers: egui::Modifiers::NONE,
            physical_key: None,
        };
        use egui::Key::*;
        for k in [Enter, Space] {
            assert!(activation_repeat(&key(k, true, true)), "{k:?}");
            assert!(!activation_repeat(&key(k, true, false)), "{k:?} press");
            assert!(!activation_repeat(&key(k, false, false)), "{k:?} release");
        }
        for k in [ArrowDown, Tab, Backspace] {
            assert!(!activation_repeat(&key(k, true, true)), "{k:?}");
        }
    }

    #[test]
    fn reduced_motion_survives_a_theme_change() {
        let ctx = egui::Context::default();
        install(&ctx, Theme::Dark);
        set_reduced_motion(&ctx, true);
        for theme in [Theme::Light, Theme::Dark, Theme::Light] {
            set_theme(&ctx, theme);
            ctx.all_styles_mut(|s| {
                assert_eq!(s.animation_time, 0.0, "{theme:?}");
                assert_eq!(s.scroll_animation, egui::style::ScrollAnimation::none());
            });
            assert!(kit(&ctx).reduced_motion);
        }
        set_reduced_motion(&ctx, false);
        ctx.all_styles_mut(|s| assert_eq!(s.animation_time, tokens::duration::FAST));
    }
}
