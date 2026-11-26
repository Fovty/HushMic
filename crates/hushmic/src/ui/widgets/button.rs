//! Buttons (`.hm-btn`): primary (ink-filled, one per view), secondary
//! (outlined, the default), ghost (toolbars, low priority), danger (the
//! secondary's outline in the danger colour: a question's act that takes
//! something away, as Quit and Turn off). Sizes sm (32 px,
//! dense app rows) and md (40 px). Weight 500 in the app; 8 px radius; an
//! optional 16 px icon 8 px before the label; disabled at 40 %; pressed
//! moves 1 px down. Never cyan.

use eframe::egui::{
    self, vec2, Color32, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo, WidgetType,
};

use super::super::fonts::Type;
use super::super::icons::{self, Icon};
use super::super::text;
use super::super::tokens::{duration, radius};
use super::{anim, backdrop, faded, focus_ring, hovered, lerp_color, pressed, shows_focus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// Two heights, each with its use (UX panel D2: 28, 32 and 40 on one
/// screen looked unplanned).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonSize {
    /// A list row's button (Files, round 3b): 28 px, padding 10, 13 px.
    /// Going: Files' rows take `Sm`.
    Xs,
    /// 32 px: an action in a card, a list row, a toolbar or a dialog (Add
    /// files…, Copy diagnostics, Reset, Cancel · Quit), and first run's
    /// Skip.
    Sm,
    /// 40 px: a page's own actions: the header's (Turn on, Mute), Live's
    /// replay row, first run's step buttons (Back, Continue, Done).
    Md,
}

/// The alarm look's fill: this much of the danger colour over the surface,
/// more under the pointer.
const ALARM_FILL: f32 = 0.18;
const ALARM_FILL_HOVER: f32 = 0.26;
/// The danger button's tint under the pointer.
const DANGER_FILL_HOVER: f32 = 0.14;
/// The primary button's lift under the pointer: a halo this wide, of the
/// ink at this opacity.
const HALO: f32 = 3.0;
const HALO_ALPHA: f32 = 0.18;
/// Pressed, the primary fill moves this far toward the muted ink.
const PRIMARY_PRESSED_DIM: f32 = 0.25;

/// A primary button held down: a little dimmer than at rest, so it reads
/// apart from hover (which brightens).
pub fn primary_pressed(pal: &crate::ui::tokens::Palette) -> Color32 {
    lerp_color(pal.ink, pal.ink_muted, PRIMARY_PRESSED_DIM)
}

/// A primary button under the pointer: the strongest ink there is, white
/// in dark and black in light.
pub fn primary_hover(pal: &crate::ui::tokens::Palette) -> Color32 {
    match pal.theme {
        crate::ui::tokens::Theme::Dark => Color32::WHITE,
        crate::ui::tokens::Theme::Light => Color32::BLACK,
    }
}

#[must_use]
pub struct Button<'a> {
    label: &'a str,
    variant: ButtonVariant,
    size: ButtonSize,
    icon: Option<Icon>,
    enabled: bool,
    /// Shown as held down (the replay row's hold button while the original plays).
    held: bool,
    /// Live's hold button (`.hm-app-hold`): the secondary look with
    /// plain label text (no tracking, no component font features).
    hold: bool,
    /// Toggle buttons (Mute): announced as pressed / not pressed.
    pressed: Option<bool>,
    /// Pressed in the danger tone, unmistakably (Mute while muted, UX
    /// panel B18): filled with a danger tint, its border, label and icon
    /// in danger.
    alarm: bool,
    /// Hide the label (icon-only); the label stays the accessible name.
    icon_only: bool,
    /// The icon after the label (a menu's chevron), not before it.
    icon_after: bool,
    min_width: f32,
    /// `aria-disabled` (Files' "Ready in …"): it stays in the tab order and
    /// is announced as unavailable, at opacity .45, and does nothing.
    dim: bool,
}

impl<'a> Button<'a> {
    pub fn new(variant: ButtonVariant, label: &'a str) -> Button<'a> {
        Button {
            label,
            variant,
            size: ButtonSize::Md,
            icon: None,
            enabled: true,
            held: false,
            hold: false,
            pressed: None,
            alarm: false,
            icon_only: false,
            icon_after: false,
            min_width: 0.0,
            dim: false,
        }
    }
    pub fn primary(label: &'a str) -> Button<'a> {
        Button::new(ButtonVariant::Primary, label)
    }
    pub fn secondary(label: &'a str) -> Button<'a> {
        Button::new(ButtonVariant::Secondary, label)
    }
    pub fn ghost(label: &'a str) -> Button<'a> {
        Button::new(ButtonVariant::Ghost, label)
    }
    /// A question's act that takes something away (Quit, Turn off; UX
    /// panel round 2, A19): never the white primary.
    pub fn danger(label: &'a str) -> Button<'a> {
        Button::new(ButtonVariant::Danger, label)
    }
    /// Live's "Hold to hear the original" (md, secondary look); pair
    /// with [`Self::held`] while the original plays.
    pub fn hold(label: &'a str) -> Button<'a> {
        Button {
            hold: true,
            ..Button::new(ButtonVariant::Secondary, label)
        }
    }
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }
    pub fn xs(self) -> Self {
        self.size(ButtonSize::Xs)
    }

    pub fn sm(self) -> Self {
        self.size(ButtonSize::Sm)
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    /// The icon 8 px after the label: a button that opens a menu, its
    /// chevron trailing as a select field's does.
    pub fn icon_after(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self.icon_after = true;
        self
    }
    /// Only the icon shows; `label` is its accessible name.
    pub fn icon_only(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self.icon_only = true;
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    /// See the `dim` field: the caller ignores its clicks.
    pub fn dim(mut self, on: bool) -> Self {
        self.dim = on;
        self
    }
    pub fn held(mut self, on: bool) -> Self {
        self.held = on;
        self
    }
    pub fn pressed(mut self, on: bool) -> Self {
        self.pressed = Some(on);
        self
    }
    /// [`Self::pressed`], shown in the danger tone while on (see `alarm`).
    pub fn alarm(mut self, on: bool) -> Self {
        self.pressed = Some(on);
        self.alarm = on;
        self
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }

    /// Rest and hover: (fill, fill under the pointer, border, its hover,
    /// ink, its hover), over the surface `behind`.
    fn tones(
        &self,
        pal: &crate::ui::tokens::Palette,
        behind: Color32,
    ) -> (Color32, Color32, Color32, Color32, Color32, Color32) {
        match self.variant {
            // hover brightens (white in dark, black in light: further from
            // the page, never toward grey) and lifts (a halo below);
            // pressed sinks a pixel and dims a little (UX panel B5)
            ButtonVariant::Primary => (
                pal.ink,
                primary_hover(pal),
                Color32::TRANSPARENT,
                Color32::TRANSPARENT,
                pal.bg,
                pal.bg,
            ),
            _ if self.alarm => (
                lerp_color(behind, pal.danger, ALARM_FILL),
                lerp_color(behind, pal.danger, ALARM_FILL_HOVER),
                pal.danger,
                pal.danger,
                pal.danger,
                pal.danger,
            ),
            // held: surface-raised, but the pointer's hover fill wins (CSS
            // `:hover:not(:disabled)` outranks `.is-down`)
            ButtonVariant::Secondary if self.held => (
                pal.button_held(),
                pal.surface_hover,
                pal.ink,
                pal.ink,
                pal.ink,
                pal.ink,
            ),
            ButtonVariant::Secondary => (
                pal.button_fill(),
                pal.surface_hover,
                pal.line_strong,
                pal.ink,
                pal.ink,
                pal.ink,
            ),
            ButtonVariant::Ghost => (
                Color32::TRANSPARENT,
                pal.surface_hover,
                Color32::TRANSPARENT,
                Color32::TRANSPARENT,
                pal.ink_muted,
                pal.ink,
            ),
            // outlined in the danger colour; a danger tint under the pointer
            ButtonVariant::Danger => (
                Color32::TRANSPARENT,
                lerp_color(behind, pal.danger, DANGER_FILL_HOVER),
                pal.danger,
                pal.danger,
                pal.danger,
                pal.danger,
            ),
        }
    }

    fn ty(&self) -> Type {
        if self.hold {
            return Type::LABEL;
        }
        match self.size {
            ButtonSize::Xs | ButtonSize::Sm => Type::BUTTON_SM,
            ButtonSize::Md => Type::BUTTON,
        }
    }

    /// The space between the border and the content, each side.
    fn pad(&self) -> f32 {
        match self.size {
            ButtonSize::Xs => 10.0,
            ButtonSize::Sm => 12.0,
            ButtonSize::Md => 16.0,
        }
    }

    /// The width this button takes: its content, the padding and the 1 px
    /// border on each side (CSS: a fixed height includes the border, an
    /// auto width adds it).
    pub fn width(&self, ctx: &egui::Context) -> f32 {
        let pad = self.pad();
        let icon: f32 = if self.icon.is_some() { 16.0 } else { 0.0 };
        if self.icon_only {
            return (2.0 + 2.0 * pad + icon).max(self.min_width);
        }
        let gap = if self.icon.is_some() { 8.0 } else { 0.0 };
        (2.0 + 2.0 * pad + icon + gap + text::width(ctx, self.label, self.ty())).max(self.min_width)
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let kit = super::super::kit(ui.ctx());
        let pal = kit.pal;
        let h = match self.size {
            ButtonSize::Xs => 28.0,
            ButtonSize::Sm => 32.0,
            ButtonSize::Md => 40.0,
        };
        let w = self.width(ui.ctx());
        let sense = if self.dim {
            Sense::focusable_noninteractive()
        } else if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense);
        let label = self.label.to_owned();
        let (enabled, toggled) = (self.enabled && !self.dim, self.pressed);
        resp.widget_info(|| match toggled {
            Some(p) => WidgetInfo::selected(WidgetType::Button, enabled, p, &label),
            None => WidgetInfo::labeled(WidgetType::Button, enabled, &label),
        });
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        // `.hm-app-hold` is no `.hm-btn`: no transition, no press offset
        let hover = anim(
            ui.ctx(),
            resp.id.with("hover"),
            self.enabled && !self.dim && hovered(&resp),
            if self.hold { 0.0 } else { duration::FAST },
        );
        let behind = backdrop(ui);
        let (fill, fill_hover, border, border_hover, ink, ink_hover) = self.tones(&pal, behind);
        let mut fill = lerp_color(fill, fill_hover, hover);
        let mut border = lerp_color(border, border_hover, hover);
        let mut ink = lerp_color(ink, ink_hover, hover);
        if !self.enabled || self.dim {
            // opacity .4 (.45 dimmed) on the whole button: each part mixed
            // against the surface behind, opaque, so the label and icon
            // fade as one
            let k = if self.dim { 0.45 } else { 0.4 };
            let under = behind.blend(fill);
            border = faded(behind, under, border, k);
            ink = faded(behind, under, ink, k);
            fill = faded(behind, behind, fill, k);
        }
        let down = self.enabled && !self.dim && !self.hold && pressed(&resp);
        let r = if down {
            rect.translate(vec2(0.0, 1.0))
        } else {
            rect
        };
        let primary = self.variant == ButtonVariant::Primary && self.enabled && !self.dim;
        if primary && down {
            fill = primary_pressed(&pal);
        }
        let p = ui.painter();
        if primary && hover > 0.0 && !down {
            // the lift: a soft halo of the ink under the button
            p.rect_filled(
                r.expand(HALO).translate(vec2(0.0, 1.0)),
                radius::SM + HALO,
                pal.ink.gamma_multiply(HALO_ALPHA * hover),
            );
        }
        p.rect(
            r,
            radius::SM,
            fill,
            Stroke::new(1.0_f32, border),
            StrokeKind::Inside,
        );
        let pad = self.pad();
        let mut x = r.left() + 1.0 + pad;
        let content_w = w - 2.0 - 2.0 * pad;
        if self.icon_only {
            if let Some(icon) = self.icon {
                icons::paint(p, icon, r.center(), 16.0, ink);
            }
        } else {
            let block = text::layout(ui.ctx(), self.label, self.ty(), ink, f32::INFINITY);
            let natural = block.size().x + if self.icon.is_some() { 24.0 } else { 0.0 };
            x += ((content_w - natural) / 2.0).max(0.0);
            if self.icon_after {
                let tw = block.size().x;
                block.paint_centered_y(p, x, r.center().y);
                if let Some(icon) = self.icon {
                    icons::paint(p, icon, egui::pos2(x + tw + 16.0, r.center().y), 16.0, ink);
                }
            } else {
                if let Some(icon) = self.icon {
                    icons::paint(p, icon, egui::pos2(x + 8.0, r.center().y), 16.0, ink);
                    x += 24.0;
                }
                block.paint_centered_y(p, x, r.center().y);
            }
        }
        if shows_focus(&kit, &resp) {
            focus_ring(p, rect, radius::SM, pal.focus_ring());
        }
        if self.enabled && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tokens::{Palette, Theme};

    /// WCAG relative luminance contrast of two opaque colours.
    fn contrast(a: Color32, b: Color32) -> f32 {
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

    #[test]
    fn a_primary_button_brightens_under_the_pointer_and_pressed_is_apart() {
        for theme in [Theme::Dark, Theme::Light] {
            let pal = Palette::of(theme);
            let (rest, hover, ..) = Button::primary("Go").tones(&pal, pal.bg);
            // never toward grey: further from the page than at rest
            assert!(
                contrast(hover, pal.bg) >= contrast(rest, pal.bg),
                "{theme:?}: hover {hover:?} vs rest {rest:?}"
            );
            assert_ne!(hover, rest, "{theme:?}: hover shows");
            let down = primary_pressed(&pal);
            assert_ne!(down, hover, "{theme:?}");
            assert_ne!(down, rest, "{theme:?}");
            // the label stays readable pressed
            assert!(contrast(down, pal.bg) >= 4.5, "{theme:?}");
        }
    }

    #[test]
    fn mute_while_muted_is_filled_in_the_danger_tone() {
        for theme in [Theme::Dark, Theme::Light] {
            let pal = Palette::of(theme);
            let (fill, _, border, _, ink, _) = Button::secondary("Mute")
                .alarm(true)
                .tones(&pal, pal.surface);
            assert_ne!(fill, pal.surface, "{theme:?}: filled");
            assert_eq!((border, ink), (pal.danger, pal.danger), "{theme:?}");
            assert!(
                contrast(ink, fill) >= 3.0,
                "{theme:?}: {:.2}",
                contrast(ink, fill)
            );
            // off: the plain secondary look
            let (fill, ..) = Button::secondary("Mute")
                .alarm(false)
                .tones(&pal, pal.surface);
            assert_eq!(fill, pal.button_fill());
        }
        // no fill in dark; in light a step lighter than the card it is on
        assert_eq!(Palette::of(Theme::Dark).button_fill(), Color32::TRANSPARENT);
        let light = Palette::of(Theme::Light);
        assert_eq!(light.button_fill(), light.surface_raised);
        assert_ne!(light.button_held(), light.button_fill());
    }
}
