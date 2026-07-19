//! About window (`--about`, tray "About HushMic\u{2026}"): a compact,
//! fixed-size brand card built like the A/B window's egui layer (eframe
//! 0.33, glow) — same visual tokens, no audio, no tray, no instance lock.

use eframe::egui::{
    self, pos2, vec2, Button, Color32, Margin, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureId, TextureOptions, Ui,
};
use std::time::Duration;

// Fixed window size; the height is pinned by the window_fits_content_height
// test (measured with the same content probe as the A/B window).
const WINDOW_SIZE: [f32; 2] = [400.0, 345.0];

// Logo edge in px (the 256 px asset, GPU-downscaled).
const LOGO_SIZE: f32 = 96.0;

// Visual tokens shared with the A/B window (abtest/ui.rs keeps its own
// copies; both use the same values).
const CARD_BG: Color32 = Color32::from_rgb(0x0d, 0x14, 0x20);
const ACCENT: Color32 = Color32::from_rgb(0x00, 0xf0, 0xf8);
const TEXT: Color32 = Color32::from_rgb(0xdc, 0xe6, 0xf2);
const TEXT_SOFT: Color32 = Color32::from_rgb(0xae, 0xbd, 0xd2);
const MUTED: Color32 = Color32::from_rgb(0x7d, 0x8c, 0xa1);
const TRACK_BG: Color32 = Color32::from_rgb(0x12, 0x1b, 0x2a);

fn with_alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn border(alpha: f32) -> Color32 {
    with_alpha(Color32::from_rgb(126, 166, 214), alpha)
}

/// "Copy diagnostics" lifecycle: collect off-thread (the probes shell out
/// to pw-dump — never block a frame), copy on arrival, confirm briefly.
enum CopyState {
    Idle,
    Collecting(std::sync::mpsc::Receiver<String>),
    /// Seconds the "Copied ✓" confirmation stays up.
    Copied(f32),
}

struct AboutApp {
    /// KWin remembers per-app window sizes (Wayland AND Xwayland) and can
    /// map us at another window's size, overriding the creation request —
    /// and a re-assert sent before the compositor's first configure lands
    /// gets overridden too. Retry while the real size disagrees, a bounded
    /// number of times (a tiling compositor may legitimately refuse).
    size_asserts_left: u8,
    size_assert_cooldown: f32,
    /// The embedded product logo (uploaded once).
    logo_tex: Option<TextureHandle>,
    /// Set by the Close button or Esc; `ui` answers with ViewportCommand::Close.
    close_requested: bool,
    /// Produces the diagnostics report text; a fn pointer so the kittest
    /// harness can inject a stub instead of probing live PipeWire.
    collector: fn() -> String,
    copy_state: CopyState,
}

fn collect_report_text() -> String {
    crate::diagnostics::render(&crate::diagnostics::collect()).0
}

impl AboutApp {
    fn new() -> Self {
        Self::with_collector(collect_report_text)
    }

    fn with_collector(collector: fn() -> String) -> Self {
        AboutApp {
            size_asserts_left: 3,
            size_assert_cooldown: 0.0,
            logo_tex: None,
            close_requested: false,
            collector,
            copy_state: CopyState::Idle,
        }
    }

    /// The centered logo: the same embedded PNG the window icon uses. None
    /// only if the embedded PNG fails to decode — the card then shows a
    /// soft accent placeholder instead.
    fn logo_texture(&mut self, ctx: &egui::Context) -> Option<TextureId> {
        if self.logo_tex.is_none() {
            let icon = crate::branding::window_icon()?;
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            // Mipmapped: the 256 px asset is drawn at 96 px, and plain
            // bilinear minification would shred the logo's thin border.
            let opts = TextureOptions {
                mipmap_mode: Some(egui::TextureFilter::Linear),
                ..TextureOptions::LINEAR
            };
            self.logo_tex = Some(ctx.load_texture("about_logo", image, opts));
        }
        self.logo_tex.as_ref().map(|t| t.id())
    }

    /// One full frame: size re-assert, Esc handling, render. Split from
    /// `App::update` so the kittest harness can drive it.
    fn ui(&mut self, ctx: &egui::Context) {
        let inner = ctx.input(|i| i.content_rect().size());
        if self.size_asserts_left > 0
            && self.size_assert_cooldown <= 0.0
            && ((inner.x - WINDOW_SIZE[0]).abs() > 1.0 || (inner.y - WINDOW_SIZE[1]).abs() > 1.0)
        {
            self.size_asserts_left -= 1;
            self.size_assert_cooldown = 0.5;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(
                WINDOW_SIZE[0],
                WINDOW_SIZE[1],
            )));
        }
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        self.size_assert_cooldown = (self.size_assert_cooldown - dt).max(0.0);

        // Copy-diagnostics lifecycle: poll the worker, copy on arrival,
        // let the confirmation decay back to idle.
        self.copy_state = match std::mem::replace(&mut self.copy_state, CopyState::Idle) {
            CopyState::Collecting(rx) => match rx.try_recv() {
                Ok(text) => {
                    #[cfg(test)]
                    ctx.data_mut(|d| d.insert_temp(egui::Id::new("t_about_copied"), text.clone()));
                    ctx.copy_text(text);
                    ctx.request_repaint_after(Duration::from_millis(100));
                    CopyState::Copied(2.0)
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(33));
                    CopyState::Collecting(rx)
                }
                // Collector thread died: give the button back rather than
                // wedge it on "Collecting…" forever.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => CopyState::Idle,
            },
            CopyState::Copied(t) if t - dt <= 0.0 => CopyState::Idle,
            CopyState::Copied(t) => {
                ctx.request_repaint_after(Duration::from_millis(100));
                CopyState::Copied(t - dt)
            }
            idle => idle,
        };

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.close_requested = true;
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(CARD_BG)
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.visuals_mut().hyperlink_color = ACCENT;
                // Scoped so the content height is measurable (the panel ui
                // itself always expands to the full window).
                let _content = ui.vertical_centered(|ui| self.content(ui));
                // Content-height probe for the fixed-window-size test.
                #[cfg(test)]
                ui.ctx().data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new("t_about_content_bottom"),
                        _content.response.rect.bottom(),
                    )
                });
            });

        if self.close_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        #[cfg(test)]
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new("t_about_close_requested"),
                self.close_requested,
            )
        });

        // The size re-assert countdown needs frames even without input.
        if self.size_assert_cooldown > 0.0 {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    fn content(&mut self, ui: &mut Ui) {
        let (logo_rect, _) = ui.allocate_exact_size(vec2(LOGO_SIZE, LOGO_SIZE), Sense::hover());
        match self.logo_texture(ui.ctx()) {
            Some(tex_id) => {
                // The chrome (rounded navy square, cyan border) is baked
                // into the image itself.
                ui.painter().image(
                    tex_id,
                    logo_rect,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                ui.painter()
                    .rect_filled(logo_rect, 22.0, with_alpha(ACCENT, 0.10));
            }
        }
        ui.add_space(14.0);
        ui.label(RichText::new("HushMic").size(22.0).color(TEXT).strong());
        ui.label(
            RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                .size(12.0)
                .color(MUTED),
        );
        ui.add_space(10.0);
        ui.label(
            RichText::new("Real-time noise suppression for Linux")
                .size(12.5)
                .color(TEXT_SOFT),
        );
        ui.add_space(14.0);
        ui.label(RichText::new("MIT OR Apache-2.0").size(11.0).color(MUTED));
        ui.add_space(2.0);
        ui.hyperlink_to(
            RichText::new("github.com/Fovty/hushmic").size(11.5),
            "https://github.com/Fovty/hushmic",
        );
        ui.hyperlink_to(
            RichText::new("Noise model: DPDFNet").size(11.5),
            "https://github.com/ceva-ip/DPDFNet",
        );
        ui.add_space(16.0);
        // Same breathing room as the A/B window's transport buttons.
        ui.spacing_mut().button_padding = vec2(14.0, 7.0);
        // Plain text states — no glyphs (U+2713 renders as tofu where
        // egui's bundled fonts lack coverage); the confirmation reads in
        // the accent color instead.
        let (copy_label, copy_color) = match self.copy_state {
            CopyState::Idle => ("Copy diagnostics", TEXT),
            CopyState::Collecting(_) => ("Collecting…", TEXT),
            CopyState::Copied(_) => ("Copied", ACCENT),
        };
        let copy = ui.add_enabled(
            matches!(self.copy_state, CopyState::Idle),
            Button::new(RichText::new(copy_label).size(12.0).color(copy_color))
                .fill(TRACK_BG)
                .stroke(Stroke::new(1.0_f32, border(0.16)))
                .corner_radius(7.0)
                .min_size(vec2(150.0, 32.0)),
        );
        if copy.clicked() {
            let (tx, rx) = std::sync::mpsc::channel();
            let collector = self.collector;
            std::thread::spawn(move || {
                let _ = tx.send(collector());
            });
            self.copy_state = CopyState::Collecting(rx);
        }
        // No Close button: Esc and the WM titlebar close the card.
    }
}

impl eframe::App for AboutApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }
}

/// Open the About window and block until it closes.
pub fn run() -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("About HushMic")
        .with_app_id("hushmic")
        .with_inner_size(WINDOW_SIZE)
        .with_min_inner_size(WINDOW_SIZE)
        .with_max_inner_size(WINDOW_SIZE)
        .with_resizable(false);
    if let Some(icon) = crate::branding::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "About HushMic",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(AboutApp::new()))
        }),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn harness(size: egui::Vec2) -> egui_kittest::Harness<'static> {
        let mut app = AboutApp::new();
        egui_kittest::Harness::builder()
            .with_size(size)
            .build(move |ctx| app.ui(ctx))
    }

    fn close_requested(harness: &egui_kittest::Harness<'static>) -> bool {
        harness
            .ctx
            .data(|d| d.get_temp(egui::Id::new("t_about_close_requested")))
            .expect("close probe must be set")
    }

    // The window is fixed-size: the content (probe: bottom of the centered
    // column) plus the 24 px frame margin must fit the pinned height — and
    // stay tight, so no dead strip creeps in.
    #[test]
    fn window_fits_content_height() {
        // Oversized on purpose: measure the content, don't clip it.
        let mut harness = harness(vec2(WINDOW_SIZE[0], 900.0));
        harness.run_steps(2);
        let bottom: f32 = harness
            .ctx
            .data(|d| d.get_temp(egui::Id::new("t_about_content_bottom")))
            .expect("content probe must be set");
        let required = bottom + 24.0; // bottom inner margin
        assert!(
            required <= WINDOW_SIZE[1],
            "content needs {required} px, window height is {}",
            WINDOW_SIZE[1]
        );
        assert!(
            WINDOW_SIZE[1] - required < 24.0,
            "dead strip: content ends at {required} px in a {} px window",
            WINDOW_SIZE[1]
        );
    }

    // kittest Tier-A: the identity lines are reachable by label (kittest
    // panics on a missing node), and Close / Esc both request the close.
    #[test]
    fn shows_version_and_identity_lines() {
        let mut harness = harness(vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]));
        harness.run_steps(2);
        harness.get_by_label(&format!("Version {}", env!("CARGO_PKG_VERSION")));
        harness.get_by_label("Real-time noise suppression for Linux");
        harness.get_by_label("MIT OR Apache-2.0");
    }

    fn harness_with_collector(collector: fn() -> String) -> egui_kittest::Harness<'static> {
        let mut app = AboutApp::with_collector(collector);
        egui_kittest::Harness::builder()
            .with_size(vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
            .build(move |ctx| app.ui(ctx))
    }

    /// Drive frames until the copy probe appears (the collect runs on a
    /// worker thread, so arrival is not frame-deterministic).
    fn wait_for_copy(harness: &mut egui_kittest::Harness<'static>) -> Option<String> {
        for _ in 0..100 {
            harness.run_steps(1);
            let copied: Option<String> = harness
                .ctx
                .data(|d| d.get_temp(egui::Id::new("t_about_copied")));
            if copied.is_some() {
                return copied;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn copy_diagnostics_button_copies_the_report() {
        let mut harness = harness_with_collector(|| "stub diagnostics report".into());
        harness.run_steps(2);
        harness.get_by_label("Copy diagnostics").click();
        let copied = wait_for_copy(&mut harness);
        assert_eq!(copied.as_deref(), Some("stub diagnostics report"));
        // The button confirms in plain text — no glyphs: egui's bundled
        // fonts don't cover U+2713 everywhere (rendered as tofu on Arch).
        harness.run_steps(2);
        harness.get_by_label("Copied");
    }

    // No Close button by design (2026-07-19): Esc and the WM titlebar
    // close the card; a button was redundant chrome.
    #[test]
    fn no_close_button_rendered() {
        let harness = {
            let mut h = harness(vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]));
            h.run_steps(2);
            h
        };
        assert!(
            harness.query_by_label("Close").is_none(),
            "Close button should be gone"
        );
    }

    #[test]
    fn escape_requests_close() {
        let mut harness = harness(vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]));
        harness.run_steps(2);
        assert!(!close_requested(&harness));
        harness.key_press(egui::Key::Escape);
        harness.run_steps(2);
        assert!(close_requested(&harness));
    }
}
