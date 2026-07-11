//! egui layer of the A/B window (eframe 0.33, glow). Renders the
//! side-by-side layout from `WindowState` + the backend frame stream; owns
//! no audio state — everything crosses the two channels.

use crate::abtest::audio::Backend;
use crate::abtest::dsp;
use crate::abtest::state::{Controls, Mode, Status, WindowState};
use crate::abtest::types::{Channel, Command, Frame, DB_FLOOR, FREQ_HI, FREQ_LO, RECORD_SECS};
use eframe::egui::{
    self, pos2, text::LayoutJob, vec2, Align, Align2, Button, Color32, ColorImage, FontId, Id,
    Label, LayerId, Layout, Margin, Mesh, Order, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, TextFormat, TextureHandle, TextureId, TextureOptions, Ui, Vec2,
};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

// Fixed window size; the height is pinned by the window_fits_content_height
// test (no reserved bottom strip — the toast overlays content instead). The
// width leaves room for the Hz gutter and the meter's dB scale on each pane
// without shrinking the spectrograms.
const WINDOW_SIZE: [f32; 2] = [1020.0, 544.0];
/// Left gutter of each pane: right-aligned Hz labels + tick. Sized for the
/// widest label ("500 Hz"/"120 Hz") at the 10.5 px axis font plus the 8 px
/// text offset, so nothing clips the pane's left edge.
const HZ_GUTTER_W: f32 = 50.0;
/// Right gutter of each pane: the meter's "+0 dB" / "−80 dB" scale. Sized
/// for the widest label ("−80 dB") at the 10.5 px font plus the 5 px offset.
const DB_GUTTER_W: f32 = 44.0;

// Spectrogram backing image (fixed; drawn stretched to the panel).
const SPEC_W: usize = 420;
const SPEC_H: usize = 212;
// Full-take sample spectrogram column cap (RECORD_SECS × SPECTRUM_HZ ≈ 300
// columns expected; headroom for cadence jitter).
const SAMPLE_COLS_MAX: usize = 400;
// Click guard when "Go live" appears in the slot Stop just vacated.
const GO_LIVE_COOLDOWN_SECS: f32 = 0.35;
// Two-ramp palette resolution (33 discrete stops).
const PALETTE_STOPS: usize = 33;
// Knee between the bg→dim and dim→hi ramps.
const RAMP_KNEE: f32 = 0.55;
const DIM_SCALE: f32 = 0.55;

// Visual tokens; the accent is the logo cyan.
const CARD_BG: Color32 = Color32::from_rgb(0x0d, 0x14, 0x20);
const SUB_PANEL_BG: Color32 = Color32::from_rgb(0x0a, 0x10, 0x1a);
const SPEC_INSET_BG: Color32 = Color32::from_rgb(0x08, 0x0d, 0x15);
const ACCENT: Color32 = Color32::from_rgb(0x00, 0xf0, 0xf8);
const ACCENT_TEXT_ON_FILL: Color32 = Color32::from_rgb(0x04, 0x22, 0x2c);
const DANGER: Color32 = Color32::from_rgb(0xe8, 0x65, 0x5a);
const RECORD_TEXT_ON_FILL: Color32 = Color32::from_rgb(0x2f, 0x0c, 0x08);
const PLAYBACK_BLUE: Color32 = Color32::from_rgb(0x7d, 0xd3, 0xfc);
const TEXT: Color32 = Color32::from_rgb(0xdc, 0xe6, 0xf2);
const TEXT_SOFT: Color32 = Color32::from_rgb(0xae, 0xbd, 0xd2);
const MUTED: Color32 = Color32::from_rgb(0x7d, 0x8c, 0xa1);
const IDLE_GRAY: Color32 = Color32::from_rgb(0x55, 0x63, 0x7a);
const TRACK_BG: Color32 = Color32::from_rgb(0x12, 0x1b, 0x2a);

// Spectrogram palette anchors (f32 so the ramp interpolates before rounding).
const SPEC_BG_RGB: [f32; 3] = [8.0, 13.0, 21.0];
const RAW_BASE_RGB: [f32; 3] = [122.0, 146.0, 176.0];
const FILTERED_BASE_RGB: [f32; 3] = [0.0, 240.0, 248.0];
const HI_RGB: [f32; 3] = [235.0, 255.0, 250.0];

// Level-meter fill gradients (bottom → full-scale top).
const RAW_METER_LO: Color32 = Color32::from_rgb(0x5b, 0x70, 0x8c);
const RAW_METER_HI: Color32 = Color32::from_rgb(0xc8, 0xd6, 0xea);
const FILTERED_METER_HI: Color32 = Color32::from_rgb(0xea, 0xff, 0xfa);

// Below this readout floor the dB label collapses to "−∞ dB".
const READOUT_INF_DB: f32 = -75.0;

fn with_alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn border(alpha: f32) -> Color32 {
    with_alpha(Color32::from_rgb(126, 166, 214), alpha)
}

fn lerp_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> Color32 {
    let ch = |i: usize| (a[i] + (b[i] - a[i]) * t).round() as u8;
    Color32::from_rgb(ch(0), ch(1), ch(2))
}

/// Two-ramp spectrogram color: bg→dim over [0, RAMP_KNEE), dim→hi over
/// [RAMP_KNEE, 1], where dim = DIM_SCALE * base.
fn ramp_color(v: f32, base: [f32; 3]) -> Color32 {
    let v = v.clamp(0.0, 1.0);
    let dim = [
        base[0] * DIM_SCALE,
        base[1] * DIM_SCALE,
        base[2] * DIM_SCALE,
    ];
    if v < RAMP_KNEE {
        lerp_rgb(SPEC_BG_RGB, dim, v / RAMP_KNEE)
    } else {
        lerp_rgb(dim, HI_RGB, (v - RAMP_KNEE) / (1.0 - RAMP_KNEE))
    }
}

fn build_palette(base: [f32; 3]) -> [Color32; PALETTE_STOPS] {
    std::array::from_fn(|i| ramp_color(i as f32 / (PALETTE_STOPS - 1) as f32, base))
}

fn palette_index(v: f32) -> usize {
    (v.clamp(0.0, 1.0) * (PALETTE_STOPS - 1) as f32).round() as usize
}

/// Nearest-bin vertical mapping; row 0 is the TOP pixel row, bins[0]
/// renders at the BOTTOM row.
fn row_bin(row: usize, rows: usize, bins: usize) -> usize {
    if rows <= 1 || bins == 0 {
        return 0;
    }
    let from_bottom = (rows - 1 - row.min(rows - 1)) as f32 / (rows - 1) as f32;
    ((from_bottom * (bins - 1) as f32).round() as usize).min(bins - 1)
}

/// Meter readout: "−∞ dB" below READOUT_INF_DB, else one decimal (U+2212).
fn format_level_db(db: f32) -> String {
    if db < READOUT_INF_DB {
        "\u{2212}\u{221e} dB".to_string()
    } else if db < 0.0 {
        format!("\u{2212}{:.1} dB", -db)
    } else {
        format!("{db:.1} dB")
    }
}

fn chan_idx(ch: Channel) -> usize {
    match ch {
        Channel::Raw => 0,
        Channel::Filtered => 1,
    }
}

/// Per-channel scrolling spectrogram: a column ring buffer straightened
/// into a fixed ColorImage on every new column, uploaded via TextureHandle.
struct Spectrogram {
    columns: Vec<[Color32; SPEC_H]>,
    // Oldest slot once the ring is full (also the next overwrite target).
    head: usize,
    palette: [Color32; PALETTE_STOPS],
    image: ColorImage,
    tex: Option<TextureHandle>,
    dirty: bool,
    tex_name: &'static str,
}

impl Spectrogram {
    fn new(base: [f32; 3], tex_name: &'static str) -> Self {
        let palette = build_palette(base);
        Spectrogram {
            columns: Vec::with_capacity(SPEC_W),
            head: 0,
            palette,
            image: ColorImage::filled([SPEC_W, SPEC_H], palette[0]),
            tex: None,
            dirty: true,
            tex_name,
        }
    }

    fn push_bins(&mut self, bins: &[f32]) {
        if bins.is_empty() {
            return;
        }
        let mut col = [self.palette[0]; SPEC_H];
        for (row, px) in col.iter_mut().enumerate() {
            let v = dsp::db_to_unit(bins[row_bin(row, SPEC_H, bins.len())]);
            *px = self.palette[palette_index(v)];
        }
        if self.columns.len() < SPEC_W {
            self.columns.push(col);
        } else {
            self.columns[self.head] = col;
            self.head = (self.head + 1) % SPEC_W;
        }
        self.rebuild();
        self.dirty = true;
    }

    /// Straighten the ring into the image, newest column at the right edge.
    fn rebuild(&mut self) {
        let n = self.columns.len();
        for x in 0..SPEC_W {
            let logical = x as isize - (SPEC_W as isize - n as isize);
            if logical < 0 {
                continue; // image starts bg-filled; blank columns stay bg
            }
            let idx = if n == SPEC_W {
                (self.head + logical as usize) % SPEC_W
            } else {
                logical as usize
            };
            let col = &self.columns[idx];
            for (y, px) in col.iter().enumerate() {
                self.image.pixels[y * SPEC_W + x] = *px;
            }
        }
    }

    fn texture(&mut self, ctx: &egui::Context) -> &TextureHandle {
        match &mut self.tex {
            Some(tex) => {
                if self.dirty {
                    tex.set(self.image.clone(), TextureOptions::NEAREST);
                    self.dirty = false;
                }
            }
            None => {
                self.tex = Some(ctx.load_texture(
                    self.tex_name,
                    self.image.clone(),
                    TextureOptions::NEAREST,
                ));
                self.dirty = false;
            }
        }
        self.tex.as_ref().expect("texture just ensured")
    }
}

/// Straighten a full take into one image, columns left-to-right in capture
/// order (README-demo style); an empty take is a single bg column.
fn sample_image(cols: &[Vec<f32>], palette: &[Color32; PALETTE_STOPS]) -> ColorImage {
    let w = cols.len().max(1);
    let mut image = ColorImage::filled([w, SPEC_H], palette[0]);
    for (x, bins) in cols.iter().enumerate() {
        for row in 0..SPEC_H {
            let v = dsp::db_to_unit(bins[row_bin(row, SPEC_H, bins.len())]);
            image.pixels[row * w + x] = palette[palette_index(v)];
        }
    }
    image
}

struct AbApp {
    state: WindowState,
    cmd_tx: Sender<Command>,
    frame_rx: Receiver<Frame>,
    specs: [Spectrogram; 2], // [raw, filtered]
    /// Columns of the take currently being recorded, per channel; promoted
    /// to `sample_bins` only on RecordDone, so a cancelled re-take never
    /// blanks the previous sample (which stays on disk and playable).
    pending_bins: [Vec<Vec<f32>>; 2],
    /// Full-take spectrogram columns of the last COMPLETED take, per
    /// channel; the Sample/Playback panes render the whole take at once.
    sample_bins: [Vec<Vec<f32>>; 2],
    /// Bumped per completed take so the cached sample textures rebuild
    /// lazily.
    sample_gen: u64,
    sample_tex: [Option<(u64, TextureHandle)>; 2],
    level_target: [f32; 2],
    level_shown: [f32; 2],
    /// Seconds during which "Go live" ignores clicks after it replaces the
    /// Stop button in the same transport slot (playback just ended): an
    /// in-flight click there must not turn the mic back on.
    go_live_cooldown: f32,
    /// KWin remembers per-app window sizes (Wayland AND Xwayland) and can
    /// map us at an older build's size, overriding the creation request —
    /// and a re-assert sent before the compositor's first configure lands
    /// gets overridden too. Retry while the real size disagrees, a bounded
    /// number of times (a tiling compositor may legitimately refuse).
    size_asserts_left: u8,
    size_assert_cooldown: f32,
    /// The real product icon rendered in the header (uploaded once).
    logo_tex: Option<TextureHandle>,
    /// White transport glyphs (record/play/stop), tinted per button state
    /// at draw time — one texture per shape, no per-color variants.
    icon_tex: Option<[TextureHandle; 3]>,
    /// E2E script driver (HUSHMIC_AB_SCRIPT): commands injected as if the
    /// user clicked, so optimistic transitions run through the same path.
    driver_rx: Option<Receiver<Command>>,
}

impl AbApp {
    fn new(cmd_tx: Sender<Command>, frame_rx: Receiver<Frame>) -> Self {
        AbApp {
            state: WindowState::new(),
            cmd_tx,
            frame_rx,
            specs: [
                Spectrogram::new(RAW_BASE_RGB, "abtest_spec_raw"),
                Spectrogram::new(FILTERED_BASE_RGB, "abtest_spec_filtered"),
            ],
            pending_bins: [Vec::new(), Vec::new()],
            sample_bins: [Vec::new(), Vec::new()],
            sample_gen: 0,
            sample_tex: [None, None],
            level_target: [DB_FLOOR; 2],
            level_shown: [DB_FLOOR; 2],
            go_live_cooldown: 0.0,
            size_asserts_left: 3,
            size_assert_cooldown: 0.0,
            logo_tex: None,
            icon_tex: None,
            driver_rx: None,
        }
    }

    /// The header icon: the same embedded logo the window icon uses,
    /// GPU-downscaled (LINEAR). None only if the embedded PNG fails to
    /// decode — the header then falls back to the painted glyph.
    fn logo_texture(&mut self, ctx: &egui::Context) -> Option<TextureId> {
        if self.logo_tex.is_none() {
            let icon = crate::branding::window_icon()?;
            let image = ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            // Mipmapped: the 256px asset is drawn at 34px, and plain
            // bilinear minification would shred the logo's thin border.
            let opts = TextureOptions {
                mipmap_mode: Some(egui::TextureFilter::Linear),
                ..TextureOptions::LINEAR
            };
            self.logo_tex = Some(ctx.load_texture("abtest_logo", image, opts));
        }
        self.logo_tex.as_ref().map(|t| t.id())
    }

    /// Lazy [record ◉, play ▶, stop ■] glyph textures (white, tinted at
    /// draw time). Rasterized here instead of using font glyphs: egui's
    /// bundled fonts don't reliably cover the geometric shapes, and a
    /// missing-glyph box on the transport row would be worse than no icon.
    fn icon_textures(&mut self, ctx: &egui::Context) -> [TextureId; 3] {
        let tex = self.icon_tex.get_or_insert_with(|| {
            [
                ("abtest_ic_rec", icon_record()),
                ("abtest_ic_play", icon_play()),
                ("abtest_ic_stop", icon_stop()),
            ]
            .map(|(name, img)| ctx.load_texture(name, img, TextureOptions::LINEAR))
        });
        [tex[0].id(), tex[1].id(), tex[2].id()]
    }

    fn send(&mut self, c: Command) {
        let was_playing = matches!(self.state.mode, Mode::Playback(_));
        let _ = self.cmd_tx.send(c);
        self.state.on_command(c);
        // A new take records into the pending buffers; the reviewed sample
        // is only replaced once RecordDone promotes the take (so neither a
        // refused Record nor a cancelled one touches it).
        if c == Command::Record && self.state.mode == Mode::Recording {
            self.pending_bins = [Vec::new(), Vec::new()];
        }
        // Stopping playback swaps Go live into the Stop button's spot on
        // the very next frame: hold it briefly so the second click of a
        // double-click can't turn the mic back on.
        if c == Command::Stop && was_playing {
            self.go_live_cooldown = GO_LIVE_COOLDOWN_SECS;
        }
    }

    /// Apply one backend frame: state machine first, then the UI-side
    /// streams (spectrogram columns, meter targets, device recovery).
    fn handle_frame(&mut self, f: &Frame) {
        let was_playing = matches!(self.state.mode, Mode::Playback(_));
        self.state.on_frame(f);
        match f {
            Frame::Spectrum { ch, bins } => {
                let idx = chan_idx(*ch);
                self.specs[idx].push_bins(bins);
                // A recording take additionally accumulates the columns for
                // the full-sample view (bounded by SAMPLE_COLS_MAX).
                if self.state.mode == Mode::Recording
                    && !bins.is_empty()
                    && self.pending_bins[idx].len() < SAMPLE_COLS_MAX
                {
                    self.pending_bins[idx].push(bins.clone());
                }
            }
            // Authoritative take start (the backend just armed the
            // buffers): whatever was captured before this is pre-take.
            Frame::RecordStarted => {
                self.pending_bins = [Vec::new(), Vec::new()];
            }
            // Promote the take: RecordDone always denotes a stored, playable
            // sample (see state.rs), even when a cancel raced the completion
            // — a sparse image beats panes that lie about the audio.
            Frame::RecordDone => {
                self.sample_bins = std::mem::take(&mut self.pending_bins);
                self.sample_gen += 1;
            }
            // Natural playback end swaps Go live into the Stop button's
            // spot: same click guard as the user-initiated stop above.
            Frame::PlaybackDone if was_playing => {
                self.go_live_cooldown = GO_LIVE_COOLDOWN_SECS;
            }
            Frame::Level {
                raw_db,
                filtered_db,
            } => self.level_target = [*raw_db, *filtered_db],
            // Fresh boot / post-retry recovery: resume the split view — but
            // never yank the user out of a sample review.
            Frame::Device { ok: true, .. }
                if self.state.mode == Mode::Sample && !self.state.has_sample =>
            {
                self.send(Command::StartMonitor);
            }
            _ => {}
        }
    }

    fn controls(&self) -> Controls {
        self.state.controls()
    }

    /// One full frame: drain the backend stream, advance clocks, render.
    /// Split from `App::update` so the kittest harness can drive it.
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
        // Scripted commands replay through send() like real clicks.
        while let Some(cmd) = self.driver_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.send(cmd);
        }
        while let Ok(f) = self.frame_rx.try_recv() {
            self.handle_frame(&f);
        }

        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        self.state.tick(dt);
        self.go_live_cooldown = (self.go_live_cooldown - dt).max(0.0);
        self.size_assert_cooldown = (self.size_assert_cooldown - dt).max(0.0);
        // The mic is released in the sample view: meters/readouts decay to
        // the floor instead of freezing at the last live value. During
        // playback the backend streams the take's own levels at the
        // playhead, so the meters keep working.
        if self.state.mode == Mode::Sample {
            self.level_target = [DB_FLOOR; 2];
        }
        for i in 0..2 {
            // Fast attack, slower release, framerate-independent.
            let k = if self.level_target[i] > self.level_shown[i] {
                1.0 - (-dt * 30.0).exp()
            } else {
                1.0 - (-dt * 8.0).exp()
            };
            self.level_shown[i] += (self.level_target[i] - self.level_shown[i]) * k;
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(CARD_BG)
                    .inner_margin(Margin::same(16)),
            )
            .show(ctx, |ui| {
                // Scoped so the content height is measurable (the panel ui
                // itself always expands to the full window).
                let _content = ui.vertical(|ui| {
                    self.header_row(ui);
                    ui.add_space(12.0);
                    self.panels_row(ui);
                    ui.add_space(12.0);
                    self.timeline_row(ui);
                    ui.add_space(10.0);
                    self.transport_row(ui);
                    ui.add_space(12.0);
                    self.summary_row(ui);
                });
                // Content-height probe for the fixed-window-size test.
                #[cfg(test)]
                ui.ctx().data_mut(|d| {
                    d.insert_temp(Id::new("t_content_bottom"), _content.response.rect.bottom())
                });
            });

        if !self.state.device_ok {
            self.error_overlay(ctx);
        }
        if let Some((msg, _)) = self.state.toast.clone() {
            toast_overlay(ctx, &msg);
        }

        // Pulses/scroll/toast countdown need frames even without input, and
        // the meter release keeps animating briefly after a stop.
        let meters_settling =
            (0..2).any(|i| (self.level_shown[i] - self.level_target[i]).abs() > 0.1);
        let animating = self.state.mode != Mode::Sample
            || self.state.toast.is_some()
            || !self.state.device_ok
            || meters_settling
            || self.go_live_cooldown > 0.0
            || self.size_assert_cooldown > 0.0;
        if animating {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    fn header_row(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let (icon_rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
            // The actual product logo (chrome — rounded navy square, cyan
            // border — is baked into the image itself).
            match self.logo_texture(ui.ctx()) {
                Some(tex_id) => {
                    ui.painter().image(
                        tex_id,
                        icon_rect,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                None => {
                    let p = ui.painter();
                    p.rect_filled(icon_rect, 9.0, with_alpha(ACCENT, 0.13));
                    p.rect_stroke(
                        icon_rect,
                        9.0,
                        Stroke::new(1.0_f32, with_alpha(ACCENT, 0.35)),
                        StrokeKind::Inside,
                    );
                    draw_mic_glyph(p, icon_rect.center(), 18.0, ACCENT, false);
                }
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("HushMic").size(14.0).color(TEXT).strong());
                    ui.label(RichText::new("\u{2022}").size(11.0).color(IDLE_GRAY));
                    ui.label(
                        RichText::new("Live A/B Mic Test")
                            .size(13.0)
                            .color(TEXT_SOFT),
                    );
                });
                ui.label(
                    RichText::new("Compare raw microphone input with HushMic filtered output")
                        .size(11.5)
                        .color(MUTED),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                status_pill(ui, self.state.status());
            });
        });
    }

    fn panels_row(&mut self, ui: &mut Ui) {
        let panels = ui.scope(|ui| {
            ui.columns(2, |cols| {
                self.channel_panel(&mut cols[0], Channel::Raw);
                self.channel_panel(&mut cols[1], Channel::Filtered);
            });
        });
        if self.state.stopped_hint() {
            stopped_hint_overlay(ui, panels.response.rect);
        }
    }

    /// Cached full-take texture for a channel, rebuilt when the take
    /// generation changes (the columns are stable once a take is reviewed).
    fn sample_texture(&mut self, ctx: &egui::Context, idx: usize) -> TextureId {
        let fresh = matches!(&self.sample_tex[idx], Some((g, _)) if *g == self.sample_gen);
        if !fresh {
            let names = ["abtest_sample_raw", "abtest_sample_filtered"];
            let image = sample_image(&self.sample_bins[idx], &self.specs[idx].palette);
            let tex = ctx.load_texture(names[idx], image, TextureOptions::NEAREST);
            self.sample_tex[idx] = Some((self.sample_gen, tex));
        }
        self.sample_tex[idx]
            .as_ref()
            .expect("texture just ensured")
            .1
            .id()
    }

    fn channel_panel(&mut self, ui: &mut Ui, ch: Channel) {
        let idx = chan_idx(ch);
        egui::Frame::new()
            .fill(SUB_PANEL_BG)
            .stroke(Stroke::new(1.0_f32, border(0.10)))
            .corner_radius(9.0)
            .inner_margin(Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let title = match ch {
                        Channel::Raw => "Raw microphone",
                        Channel::Filtered => "Filtered by HushMic",
                    };
                    ui.label(RichText::new(title).size(12.5).color(TEXT).strong());
                    if self.state.playing() == Some(ch) {
                        let t = ui.input(|i| i.time) as f32;
                        let a = 0.55 + 0.45 * (t * 4.4).sin();
                        ui.label(
                            RichText::new("PLAYING")
                                .font(FontId::monospace(9.5))
                                .color(with_alpha(ACCENT, a)),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // "RMS" chip rightmost (added first in RTL): the bare
                        // dB number read like a peak level without it.
                        egui::Frame::new()
                            .fill(TRACK_BG)
                            .corner_radius(4.0)
                            .inner_margin(Margin::symmetric(5, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("RMS")
                                        .font(FontId::monospace(8.5))
                                        .color(MUTED),
                                );
                            });
                        ui.label(
                            RichText::new(format_level_db(self.level_shown[idx]))
                                .font(FontId::monospace(12.0))
                                .color(TEXT_SOFT),
                        );
                    });
                });
                ui.add_space(6.0);

                let avail = ui.available_width();
                let (rect, _) = ui.allocate_exact_size(vec2(avail, SPEC_H as f32), Sense::hover());
                // [Hz gutter | spectrogram | 6 | meter 9 | 4 | dB gutter]:
                // axis labels live OUTSIDE the image (readability, user
                // request) and the meter's scale is spelled out.
                let img_rect = Rect::from_min_size(
                    pos2(rect.min.x + HZ_GUTTER_W, rect.min.y),
                    vec2(
                        avail - HZ_GUTTER_W - DB_GUTTER_W - 9.0 - 6.0 - 4.0,
                        rect.height(),
                    ),
                );
                let meter_rect = Rect::from_min_size(
                    pos2(img_rect.max.x + 6.0, rect.min.y),
                    vec2(9.0, rect.height()),
                );

                // Sample review and playback show the full recorded take;
                // live modes stream the scrolling ring.
                let show_sample = matches!(self.state.mode, Mode::Playback(_))
                    || (self.state.mode == Mode::Sample && self.state.has_sample);
                let tex_id = if show_sample {
                    self.sample_texture(ui.ctx(), idx)
                } else {
                    self.specs[idx].texture(ui.ctx()).id()
                };
                let p = ui.painter();
                p.rect_filled(img_rect, 6.0, SPEC_INSET_BG);
                p.image(
                    tex_id,
                    img_rect,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                freq_axis_labels(p, img_rect);
                // Sweeping playhead during playback (both panes; the raw and
                // filtered takes are time-aligned).
                if matches!(self.state.mode, Mode::Playback(_)) {
                    let x = (img_rect.left()
                        + (self.state.play_pos / RECORD_SECS) * img_rect.width())
                    .clamp(img_rect.left() + 1.0, img_rect.right() - 1.0);
                    p.rect_filled(
                        Rect::from_min_max(
                            pos2(x - 1.0, img_rect.top()),
                            pos2(x + 1.0, img_rect.bottom()),
                        ),
                        0.0,
                        with_alpha(Color32::WHITE, 0.75),
                    );
                }

                // Meter scale: the actual mapping (0 dBFS top, DB_FLOOR
                // bottom) — the labels reflect the real range, not a
                // decorative −120.
                for (y, align, label) in [
                    (meter_rect.top(), Align2::LEFT_TOP, "+0 dB".to_string()),
                    (
                        meter_rect.bottom(),
                        Align2::LEFT_BOTTOM,
                        format!("\u{2212}{:.0} dB", -DB_FLOOR),
                    ),
                ] {
                    p.text(
                        pos2(meter_rect.right() + 5.0, y),
                        align,
                        label,
                        FontId::monospace(10.5),
                        with_alpha(TEXT, 0.55),
                    );
                }

                p.rect_filled(meter_rect, 3.0, SPEC_INSET_BG);
                let pct = dsp::db_to_unit(self.level_shown[idx]);
                if pct > 0.0 {
                    let (lo, hi_full) = match ch {
                        Channel::Raw => (RAW_METER_LO, RAW_METER_HI),
                        Channel::Filtered => (ACCENT, FILTERED_METER_HI),
                    };
                    let top = color_lerp(lo, hi_full, pct);
                    let fill = Rect::from_min_max(
                        pos2(
                            meter_rect.left(),
                            meter_rect.bottom() - pct * meter_rect.height(),
                        ),
                        meter_rect.max,
                    );
                    vertical_gradient(p, fill, top, lo);
                }

                ui.add_space(8.0);
                let caption = match ch {
                    Channel::Raw => "Unprocessed input straight from your device",
                    Channel::Filtered => "What other applications receive",
                };
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(caption).size(11.0).color(MUTED));
                });
            });
    }

    fn timeline_row(&mut self, ui: &mut Ui) {
        let tl = self.state.timeline();
        ui.horizontal(|ui| {
            let (label_rect, _) = ui.allocate_exact_size(vec2(196.0, 14.0), Sense::hover());
            ui.painter().text(
                pos2(label_rect.left(), label_rect.center().y),
                Align2::LEFT_CENTER,
                &tl.label,
                FontId::monospace(11.0),
                MUTED,
            );
            // Reserve room for the right-aligned mono time readout: measure
            // the longest form (a fixed guess overlapped in Record mode).
            let time_w = ui
                .painter()
                .layout_no_wrap(
                    "0:00.0 / 0:10.0".to_string(),
                    FontId::monospace(11.0),
                    TEXT_SOFT,
                )
                .size()
                .x;
            let track_w = (ui.available_width() - time_w - ui.spacing().item_spacing.x).max(0.0);
            let (track_rect, _) = ui.allocate_exact_size(vec2(track_w, 14.0), Sense::hover());
            let bar = Rect::from_center_size(track_rect.center(), vec2(track_rect.width(), 4.0));
            let p = ui.painter();
            p.rect_filled(bar, 2.0, TRACK_BG);
            // Timeline.pct is a PERCENTAGE (state tests pin the 0-100 scale).
            let pct = tl.pct.clamp(0.0, 100.0) / 100.0;
            if pct > 0.0 {
                let fill = Rect::from_min_size(bar.min, vec2(bar.width() * pct, bar.height()));
                p.rect_filled(fill, 2.0, ACCENT);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(&tl.time)
                        .font(FontId::monospace(11.0))
                        .color(TEXT_SOFT),
                );
            });
        });
    }

    fn transport_row(&mut self, ui: &mut Ui) {
        let c = self.controls();
        let playing = self.state.playing();
        let [ic_rec, ic_play, ic_stop] = self.icon_textures(ui.ctx());
        ui.horizontal(|ui| {
            // Labels breathe: real padding instead of the theme's cramped
            // default (heights stay 32 via min_size).
            ui.spacing_mut().button_padding = vec2(14.0, 7.0);
            let record_label = if self.state.mode == Mode::Sample && self.state.has_sample {
                "Record new sample"
            } else {
                "Record 10 s sample"
            };
            if record_button(ui, record_label, c.record, ic_rec).clicked() {
                self.send(Command::Record);
            }
            if secondary_button(
                ui,
                "Play raw",
                c.play,
                playing == Some(Channel::Raw),
                Some(ic_play),
            )
            .clicked()
            {
                self.send(Command::Play(Channel::Raw));
            }
            if secondary_button(
                ui,
                "Play filtered",
                c.play,
                playing == Some(Channel::Filtered),
                Some(ic_play),
            )
            .clicked()
            {
                self.send(Command::Play(Channel::Filtered));
            }
            if self.state.mode == Mode::Sample
                && secondary_button(
                    ui,
                    "Go live",
                    c.go_live && self.go_live_cooldown <= 0.0,
                    false,
                    None,
                )
                .clicked()
            {
                self.send(Command::StartMonitor);
            }
            // Contextual stop: a recording take is cancelled (monitors keep
            // running), playback stops into the sample view.
            let stop_label = match self.state.mode {
                Mode::Recording => Some("Cancel"),
                Mode::Playback(_) => Some("Stop"),
                _ => None,
            };
            if let Some(label) = stop_label {
                if secondary_button(ui, label, c.stop, false, Some(ic_stop)).clicked() {
                    self.send(Command::Stop);
                }
            }
        });
    }

    fn summary_row(&mut self, ui: &mut Ui) {
        // No sample yet → no numbers to show. One quiet hint keeps the
        // layout stable (fixed window) and teaches the record flow instead
        // of parking three empty tiles under text promising measurements.
        let Some(m) = self.state.metrics else {
            card_frame(ui, |ui| {
                // card_frame pins the same inner height as the populated
                // cards: the row must not jump when the first sample's
                // numbers replace the hint.
                ui.vertical_centered(|ui| {
                    ui.add_space((SUMMARY_CARD_INNER_H - 14.0) / 2.0);
                    ui.label(
                        RichText::new(
                            "Record a 10 s sample to measure background \
                             reduction and voice retention",
                        )
                        .size(11.0)
                        .color(MUTED),
                    );
                });
            });
            return;
        };

        // Small raw→filtered component line under each header.
        let arrow_line = |ui: &mut Ui, pre: String, post: String| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let small = |s: String| RichText::new(s).font(FontId::monospace(10.0));
                ui.label(small(pre).color(TEXT_SOFT));
                ui.label(small("\u{2192}".to_string()).color(MUTED));
                ui.label(small(post).color(TEXT_SOFT));
            });
        };
        let big_line = |ui: &mut Ui, value: String, word: &str, color: Color32| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(value)
                        .font(FontId::monospace(19.0))
                        .color(color),
                );
                ui.label(RichText::new(word).size(10.5).color(MUTED));
            });
        };
        let header = |ui: &mut Ui, text: &str| {
            ui.label(
                RichText::new(text)
                    .font(FontId::monospace(9.5))
                    .color(MUTED),
            );
        };
        let dbfs = |v: f32| format!("{:.1} dBFS", v).replace('-', "\u{2212}");
        let db1 = |v: f32| format!("{:.1} dB", v).replace('-', "\u{2212}");

        ui.columns(2, |cols| {
            cols[0].scope(|ui| {
                card_frame(ui, |ui| {
                    header(ui, "BACKGROUND NOISE");
                    // The model gates pauses below our measurement floor:
                    // show "silent" and a "≥" reduction rather than the
                    // literal clamp value, which reads as a magic number.
                    let silent =
                        m.filtered_floor_dbfs <= crate::abtest::metrics::SILENT_FLOOR_DBFS + 0.05;
                    let filtered_label = if silent {
                        "silent".to_string()
                    } else {
                        format!("Filtered {}", dbfs(m.filtered_floor_dbfs))
                    };
                    arrow_line(
                        ui,
                        format!("Raw {}", dbfs(m.raw_floor_dbfs)),
                        filtered_label,
                    );
                    // Reduction is clamped ≥ 0 in metrics; accent it only
                    // when there's a real drop to celebrate.
                    let color = if m.background_reduction_db >= 1.0 {
                        ACCENT
                    } else {
                        TEXT
                    };
                    let value = if silent {
                        format!("\u{2265} {}", db1(m.background_reduction_db))
                    } else {
                        db1(m.background_reduction_db)
                    };
                    big_line(ui, value, "quieter", color);
                });
            });
            cols[1].scope(|ui| {
                card_frame(ui, |ui| {
                    header(ui, "VOICE");
                    if !m.voice_measurable {
                        // Never fabricate a voice number: say so instead. Two
                        // rows (hint at the arrow-line slot + "——" at the
                        // big-line slot) so this card pins to the same height
                        // as the others — the summary row must not jump, or
                        // grow past the fixed window, when a no-speech sample
                        // arrives.
                        ui.label(
                            RichText::new("add clear speech to measure")
                                .font(FontId::monospace(10.0))
                                .color(MUTED),
                        );
                        big_line(
                            ui,
                            "\u{2014}\u{2014}".to_string(),
                            "",
                            with_alpha(TEXT, 0.45),
                        );
                        return;
                    }
                    arrow_line(
                        ui,
                        format!("Raw {}", dbfs(m.raw_speech_dbfs)),
                        format!("Filtered {}", dbfs(m.filtered_speech_dbfs)),
                    );
                    // Kept (allow a touch of make-up either way) vs ducked.
                    if m.voice_retention_db >= -3.0 {
                        big_line(ui, "Preserved".to_string(), "", ACCENT);
                    } else {
                        big_line(ui, db1(m.voice_retention_db.abs()), "quieter", TEXT);
                    }
                });
            });
        });
    }

    fn error_overlay(&mut self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        // Dim in Order::Middle so the Foreground Area stays interactable
        // above it.
        ctx.layer_painter(LayerId::new(Order::Middle, Id::new("abtest_error_dim")))
            .rect_filled(screen, 0.0, with_alpha(Color32::from_rgb(7, 11, 18), 0.9));
        egui::Area::new(Id::new("abtest_error"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(46.0, 46.0), Sense::hover());
                    let p = ui.painter();
                    p.circle_filled(rect.center(), 23.0, with_alpha(DANGER, 0.10));
                    p.circle_stroke(
                        rect.center(),
                        23.0,
                        Stroke::new(1.0_f32, with_alpha(DANGER, 0.35)),
                    );
                    draw_mic_glyph(p, rect.center(), 22.0, DANGER, true);
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("No microphone detected")
                            .size(14.5)
                            .color(TEXT)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.add(
                        Label::new(
                            RichText::new(
                                "HushMic can't find an input device. Check that your microphone \
                                 is connected and that PipeWire (or PulseAudio) is running, then \
                                 try again.",
                            )
                            .size(12.0)
                            .color(MUTED),
                        )
                        .wrap(),
                    );
                    ui.add_space(10.0);
                    egui::Frame::new()
                        .fill(SUB_PANEL_BG)
                        .stroke(Stroke::new(1.0_f32, border(0.16)))
                        .corner_radius(6.0)
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("$ pactl list sources short")
                                    .font(FontId::monospace(11.0))
                                    .color(TEXT_SOFT),
                            );
                        });
                    ui.add_space(12.0);
                    // Same breathing room as the transport buttons.
                    ui.spacing_mut().button_padding = vec2(14.0, 7.0);
                    if primary_button(ui, "Retry detection", true).clicked() {
                        self.send(Command::RetryDevice);
                    }
                });
            });
    }
}

impl eframe::App for AbApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }
}

fn status_pill(ui: &mut Ui, status: Status) {
    let (label, color) = match status {
        Status::Listening => ("Listening", ACCENT),
        Status::Recording => ("Recording", DANGER),
        Status::Playback => ("Playback", PLAYBACK_BLUE),
        Status::Sample => ("Sample ready", PLAYBACK_BLUE),
        Status::Stopped => ("Stopped", IDLE_GRAY),
        Status::NoInput => ("No input", DANGER),
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), FontId::monospace(11.0), color);
    let w = 12.0 + 7.0 + 7.0 + galley.size().x + 12.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 13.0, SUB_PANEL_BG);
    p.rect_stroke(
        rect,
        13.0,
        Stroke::new(1.0_f32, border(0.16)),
        StrokeKind::Inside,
    );
    // Steady dot in the resting views (sample review / stopped); the live
    // states pulse.
    let pulse = if matches!(status, Status::Sample | Status::Stopped) {
        1.0
    } else {
        0.55 + 0.45 * (ui.input(|i| i.time) as f32 * 4.4).sin()
    };
    let dot = pos2(rect.left() + 12.0 + 3.5, rect.center().y);
    p.circle_filled(dot, 3.5, with_alpha(color, pulse));
    p.galley(
        pos2(dot.x + 3.5 + 7.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

/// Hz axis in the gutter LEFT of the spectrogram (right-aligned against the
/// image edge, with a small tick) — overlaying them on the image cost
/// readability over bright content.
fn freq_axis_labels(p: &egui::Painter, img_rect: Rect) {
    for (freq, label) in [
        (8000.0, "8 kHz"),
        (2000.0, "2 kHz"),
        (500.0, "500 Hz"),
        (120.0, "120 Hz"),
    ] {
        let unit = (freq / FREQ_LO).ln() / (FREQ_HI / FREQ_LO).ln();
        let y = (img_rect.bottom() - unit * img_rect.height())
            .clamp(img_rect.top() + 7.0, img_rect.bottom() - 7.0);
        p.text(
            pos2(img_rect.left() - 8.0, y),
            Align2::RIGHT_CENTER,
            label,
            FontId::monospace(10.5),
            with_alpha(TEXT, 0.55),
        );
        p.line_segment(
            [
                pos2(img_rect.left() - 5.0, y),
                pos2(img_rect.left() - 1.0, y),
            ],
            Stroke::new(1.0_f32, with_alpha(TEXT, 0.35)),
        );
    }
}

fn color_lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    lerp_rgb(
        [a.r() as f32, a.g() as f32, a.b() as f32],
        [b.r() as f32, b.g() as f32, b.b() as f32],
        t.clamp(0.0, 1.0),
    )
}

fn vertical_gradient(p: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(Shape::mesh(mesh));
}

fn stopped_hint_overlay(ui: &mut Ui, rect: Rect) {
    let p = ui.painter();
    p.rect_filled(rect, 9.0, with_alpha(Color32::from_rgb(7, 11, 18), 0.74));
    let c = rect.center();
    p.text(
        pos2(c.x, c.y - 16.0),
        Align2::CENTER_CENTER,
        "Not monitoring",
        FontId::proportional(13.0),
        TEXT,
    );
    let mut job = LayoutJob::default();
    job.wrap.max_width = 420.0;
    job.halign = Align::Center;
    let body = FontId::proportional(11.5);
    job.append(
        "Press ",
        0.0,
        TextFormat {
            font_id: body.clone(),
            color: MUTED,
            ..Default::default()
        },
    );
    job.append(
        "Go live",
        0.0,
        TextFormat {
            font_id: body.clone(),
            color: ACCENT,
            ..Default::default()
        },
    );
    job.append(
        " to resume the split view. Nothing is stored until you record a sample.",
        0.0,
        TextFormat {
            font_id: body,
            color: MUTED,
            ..Default::default()
        },
    );
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(pos2(c.x, c.y + 4.0), galley, MUTED);
}

/// Inner content height of a populated summary card (header + component
/// line + big line + 2×3 px spacing); the pre-sample hint pins itself to
/// the same height so the row never jumps.
const SUMMARY_CARD_INNER_H: f32 = 60.0;

/// Transport-glyph raster size; drawn at 12 px, so 2× for clean minification.
const ICON_PX: usize = 24;

/// Anti-aliased white glyph from a signed distance function (negative =
/// inside). One pixel of smoothing at the edge.
fn icon_from_sdf(sdf: impl Fn(f32, f32) -> f32) -> ColorImage {
    let mut img = ColorImage::filled([ICON_PX, ICON_PX], Color32::TRANSPARENT);
    let half = ICON_PX as f32 / 2.0;
    for y in 0..ICON_PX {
        for x in 0..ICON_PX {
            // Pixel-center coordinates in [-1, 1], y down.
            let u = (x as f32 + 0.5 - half) / half;
            let v = (y as f32 + 0.5 - half) / half;
            let d = sdf(u, v);
            let a = (0.5 - d * half).clamp(0.0, 1.0);
            img[(x, y)] = Color32::from_white_alpha((a * 255.0) as u8);
        }
    }
    img
}

/// Record: ring + center dot (the conventional ◉ take glyph).
fn icon_record() -> ColorImage {
    icon_from_sdf(|u, v| {
        let r = (u * u + v * v).sqrt();
        let ring = (r - 0.78).abs() - 0.10;
        let dot = r - 0.42;
        ring.min(dot)
    })
}

/// Play: right-pointing triangle.
fn icon_play() -> ColorImage {
    icon_from_sdf(|u, v| {
        // Half-planes of a triangle spanning x ∈ [−0.55, 0.75].
        let left = -0.55 - u;
        let upper = (v * 0.65 - (0.75 - u) * 0.35) / (0.65f32.powi(2) + 0.35f32.powi(2)).sqrt();
        let lower = (-v * 0.65 - (0.75 - u) * 0.35) / (0.65f32.powi(2) + 0.35f32.powi(2)).sqrt();
        left.max(upper).max(lower)
    })
}

/// Stop: square.
fn icon_stop() -> ColorImage {
    icon_from_sdf(|u, v| u.abs().max(v.abs()) - 0.62)
}

/// Shared summary-card chrome: identical frame for all three cards and the
/// pre-sample hint, so they read as one row.
fn card_frame(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(SUB_PANEL_BG)
        .corner_radius(8.0)
        .inner_margin(Margin::symmetric(13, 10))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.set_min_height(SUMMARY_CARD_INNER_H);
            add_contents(ui);
        });
}

fn primary_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        Button::new(
            RichText::new(label)
                .size(12.0)
                .color(ACCENT_TEXT_ON_FILL)
                .strong(),
        )
        .fill(ACCENT)
        .stroke(Stroke::NONE)
        .corner_radius(7.0)
        .min_size(vec2(0.0, 32.0)),
    )
}

/// 12 px transport glyph, tinted to match the button's text color.
fn button_icon(tex: TextureId, tint: Color32) -> egui::Image<'static> {
    egui::Image::new(egui::load::SizedTexture::new(tex, vec2(12.0, 12.0))).tint(tint)
}

/// Primary transport action: red fill (the conventional record affordance),
/// ◉ glyph, dark label for contrast on the fill.
fn record_button(ui: &mut Ui, label: &str, enabled: bool, icon: TextureId) -> egui::Response {
    ui.add_enabled(
        enabled,
        Button::image_and_text(
            button_icon(icon, RECORD_TEXT_ON_FILL),
            RichText::new(label)
                .size(12.0)
                .color(RECORD_TEXT_ON_FILL)
                .strong(),
        )
        .fill(DANGER)
        .stroke(Stroke::NONE)
        .corner_radius(7.0)
        .min_size(vec2(0.0, 32.0)),
    )
}

fn secondary_button(
    ui: &mut Ui,
    label: &str,
    enabled: bool,
    active: bool,
    icon: Option<TextureId>,
) -> egui::Response {
    // Active = this button's playback is sounding right now: full accent
    // fill with dark text, like the primary button, so the sounding button
    // is unmistakable mid-playback.
    let (fill, stroke, text_color) = if active {
        (
            ACCENT,
            Stroke::new(1.5_f32, with_alpha(ACCENT, 0.55)),
            ACCENT_TEXT_ON_FILL,
        )
    } else {
        (TRACK_BG, Stroke::new(1.0_f32, border(0.16)), TEXT)
    };
    let mut text = RichText::new(label).size(12.0).color(text_color);
    if active {
        text = text.strong();
    }
    let button = match icon {
        Some(tex) => Button::image_and_text(button_icon(tex, text_color), text),
        None => Button::new(text),
    };
    ui.add_enabled(
        enabled,
        button
            .fill(fill)
            .stroke(stroke)
            .corner_radius(7.0)
            .min_size(vec2(0.0, 32.0)),
    )
}

fn toast_overlay(ctx: &egui::Context, msg: &str) {
    // Bottom-right overlay: no reserved strip, may briefly cover content.
    egui::Area::new(Id::new("abtest_toast"))
        .order(Order::Foreground)
        .interactable(false)
        .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(TRACK_BG)
                .stroke(Stroke::new(1.0_f32, with_alpha(ACCENT, 0.4)))
                .corner_radius(7.0)
                .inner_margin(Margin::symmetric(12, 7))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(msg)
                            .font(FontId::monospace(11.0))
                            .color(ACCENT),
                    );
                });
        });
}

/// Painter-drawn mic glyph (capsule + pickup arc + stem); `off` adds the
/// diagonal strike-through. `s` is the glyph's bounding size in px.
fn draw_mic_glyph(p: &egui::Painter, c: Pos2, s: f32, color: Color32, off: bool) {
    let stroke = Stroke::new(1.6_f32, color);
    let cap = Rect::from_center_size(pos2(c.x, c.y - 0.16 * s), vec2(0.30 * s, 0.52 * s));
    p.rect_filled(cap, 0.15 * s, color);
    let arc_c = pos2(c.x, c.y + 0.02 * s);
    let r = 0.34 * s;
    let pts: Vec<Pos2> = (0..=16)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 16.0);
            pos2(arc_c.x + r * a.cos(), arc_c.y + r * a.sin())
        })
        .collect();
    p.add(Shape::line(pts, stroke));
    p.line_segment(
        [pos2(c.x, arc_c.y + r), pos2(c.x, arc_c.y + r + 0.14 * s)],
        stroke,
    );
    p.line_segment(
        [
            pos2(c.x - 0.14 * s, arc_c.y + r + 0.14 * s),
            pos2(c.x + 0.14 * s, arc_c.y + r + 0.14 * s),
        ],
        stroke,
    );
    if off {
        p.line_segment(
            [
                pos2(c.x - 0.42 * s, c.y - 0.42 * s),
                pos2(c.x + 0.42 * s, c.y + 0.42 * s),
            ],
            Stroke::new(2.0_f32, color),
        );
    }
}

/// Open the window and block until it closes. Must call
/// `backend.start(...)` once the egui context exists (the repaint callback
/// wraps `egui::Context::request_repaint`).
pub fn run(
    backend: Backend,
    cmd_tx: Sender<Command>,
    frame_rx: Receiver<Frame>,
) -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("HushMic \u{2014} Test Microphone")
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
        "HushMic \u{2014} Test Microphone",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            let repaint_ctx = cc.egui_ctx.clone();
            backend.start(Box::new(move || repaint_ctx.request_repaint()));
            let mut app = AbApp::new(cmd_tx, frame_rx);
            // Live from the first frame: monitoring runs from window open,
            // there is no Start button.
            app.send(Command::StartMonitor);
            // Hardware E2E driver: HUSHMIC_AB_SCRIPT="start,1.5,record,12,
            // play-raw,11,quit" replays a timed command sequence as if the
            // buttons were clicked (routed through the App so the optimistic
            // state transitions match real interaction).
            if let Ok(script) = std::env::var("HUSHMIC_AB_SCRIPT") {
                let (dtx, drx) = std::sync::mpsc::channel();
                app.driver_rx = Some(drx);
                let ctx = cc.egui_ctx.clone();
                std::thread::spawn(move || {
                    for tok in script.split(',') {
                        let tok = tok.trim();
                        if let Ok(secs) = tok.parse::<f32>() {
                            std::thread::sleep(Duration::from_secs_f32(secs));
                            continue;
                        }
                        let cmd = match tok {
                            "start" => Command::StartMonitor,
                            "stop" => Command::Stop,
                            "record" => Command::Record,
                            "play-raw" => Command::Play(Channel::Raw),
                            "play-filtered" => Command::Play(Channel::Filtered),
                            "retry" => Command::RetryDevice,
                            "quit" => {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                ctx.request_repaint();
                                return;
                            }
                            _ => continue,
                        };
                        let _ = dtx.send(cmd);
                        ctx.request_repaint();
                    }
                });
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abtest::types::SampleMetrics;
    use egui_kittest::kittest::Queryable;
    use std::sync::mpsc::channel;

    #[test]
    fn palette_endpoints_and_knee_raw() {
        let p = build_palette(RAW_BASE_RGB);
        assert_eq!(p[0], Color32::from_rgb(8, 13, 21));
        assert_eq!(p[PALETTE_STOPS - 1], Color32::from_rgb(235, 255, 250));
        // dim = 0.55 * (122, 146, 176) = (67.1, 80.3, 96.8), rounded.
        assert_eq!(
            ramp_color(0.55, RAW_BASE_RGB),
            Color32::from_rgb(67, 80, 97)
        );
    }

    #[test]
    fn palette_endpoints_and_knee_filtered() {
        let p = build_palette(FILTERED_BASE_RGB);
        assert_eq!(p[0], Color32::from_rgb(8, 13, 21));
        assert_eq!(p[PALETTE_STOPS - 1], Color32::from_rgb(235, 255, 250));
        // dim = 0.55 * (0, 240, 248) = (0, 132, 136.4), rounded.
        assert_eq!(
            ramp_color(0.55, FILTERED_BASE_RGB),
            Color32::from_rgb(0, 132, 136)
        );
    }

    #[test]
    fn ramp_lower_segment_interpolates_bg_to_dim() {
        // v = 0.275 is halfway through the bg→dim ramp of the raw palette:
        // (8+67.1)/2, (13+80.3)/2, (21+96.8)/2, rounded.
        assert_eq!(
            ramp_color(0.275, RAW_BASE_RGB),
            Color32::from_rgb(38, 47, 59)
        );
    }

    #[test]
    fn ramp_upper_segment_interpolates_dim_to_hi() {
        // v = 0.775 is halfway through the dim→hi ramp of the raw palette:
        // (67.1+235)/2, (80.3+255)/2, (96.8+250)/2, rounded.
        assert_eq!(
            ramp_color(0.775, RAW_BASE_RGB),
            Color32::from_rgb(151, 168, 173)
        );
    }

    #[test]
    fn ramp_clamps_out_of_range() {
        assert_eq!(
            ramp_color(-1.0, RAW_BASE_RGB),
            ramp_color(0.0, RAW_BASE_RGB)
        );
        assert_eq!(ramp_color(2.0, RAW_BASE_RGB), ramp_color(1.0, RAW_BASE_RGB));
    }

    #[test]
    fn palette_index_maps_unit_range_to_stops() {
        assert_eq!(palette_index(0.0), 0);
        assert_eq!(palette_index(1.0), PALETTE_STOPS - 1);
        assert_eq!(palette_index(0.5), 16);
        assert_eq!(palette_index(-3.0), 0);
        assert_eq!(palette_index(3.0), PALETTE_STOPS - 1);
    }

    #[test]
    fn level_db_formatting() {
        assert_eq!(format_level_db(f32::NEG_INFINITY), "\u{2212}\u{221e} dB");
        assert_eq!(format_level_db(-80.0), "\u{2212}\u{221e} dB");
        assert_eq!(format_level_db(-75.01), "\u{2212}\u{221e} dB");
        assert_eq!(format_level_db(-75.0), "\u{2212}75.0 dB");
        assert_eq!(format_level_db(-12.34), "\u{2212}12.3 dB");
        assert_eq!(format_level_db(0.0), "0.0 dB");
    }

    #[test]
    fn row_bin_maps_bottom_row_to_bin_zero() {
        assert_eq!(row_bin(SPEC_H - 1, SPEC_H, 64), 0);
        assert_eq!(row_bin(0, SPEC_H, 64), 63);
    }

    #[test]
    fn row_bin_is_monotonic_and_covers_all_bins() {
        let mut seen = [false; 64];
        let mut prev = 64usize;
        for row in 0..SPEC_H {
            let b = row_bin(row, SPEC_H, 64);
            assert!(b < 64);
            assert!(b <= prev, "bin index must not increase downward");
            seen[b] = true;
            prev = b;
        }
        assert!(seen.iter().all(|&s| s), "every bin must map to some row");
    }

    #[test]
    fn row_bin_degenerate_inputs() {
        assert_eq!(row_bin(0, 1, 64), 0);
        assert_eq!(row_bin(5, SPEC_H, 0), 0);
    }

    #[test]
    fn spectrogram_column_orientation() {
        let mut sp = Spectrogram::new(RAW_BASE_RGB, "t");
        // bins[0] loud, everything else at the floor: the bottom pixel of
        // the newest (rightmost) column must be the hi color.
        let mut bins = vec![DB_FLOOR; 64];
        bins[0] = 0.0;
        sp.push_bins(&bins);
        let hi = sp.palette[PALETTE_STOPS - 1];
        let bg = sp.palette[0];
        let x = SPEC_W - 1;
        assert_eq!(sp.image.pixels[(SPEC_H - 1) * SPEC_W + x], hi);
        assert_eq!(sp.image.pixels[x], bg);
        // Columns left of the single pushed one stay background.
        assert_eq!(sp.image.pixels[(SPEC_H - 1) * SPEC_W], bg);
    }

    #[test]
    fn spectrogram_ring_keeps_newest_at_right_edge() {
        let mut sp = Spectrogram::new(RAW_BASE_RGB, "t");
        let quiet = vec![DB_FLOOR; 64];
        let loud = vec![0.0f32; 64];
        for _ in 0..SPEC_W + 5 {
            sp.push_bins(&quiet);
        }
        sp.push_bins(&loud);
        assert_eq!(sp.columns.len(), SPEC_W);
        let hi = sp.palette[PALETTE_STOPS - 1];
        let bg = sp.palette[0];
        assert_eq!(sp.image.pixels[SPEC_W - 1], hi);
        assert_eq!(sp.image.pixels[SPEC_W - 2], bg);
    }

    #[test]
    fn spectrogram_ignores_empty_bins() {
        let mut sp = Spectrogram::new(RAW_BASE_RGB, "t");
        sp.push_bins(&[]);
        assert!(sp.columns.is_empty());
    }

    #[test]
    fn sample_image_lays_columns_left_to_right() {
        let palette = build_palette(RAW_BASE_RGB);
        // First column loud at bins[0], second quiet: bottom-left pixel is
        // the hi color, its right neighbour and the top row stay bg.
        let mut loud = vec![DB_FLOOR; 64];
        loud[0] = 0.0;
        let img = sample_image(&[loud, vec![DB_FLOOR; 64]], &palette);
        assert_eq!(img.size, [2, SPEC_H]);
        let hi = palette[PALETTE_STOPS - 1];
        let bg = palette[0];
        assert_eq!(img.pixels[(SPEC_H - 1) * 2], hi);
        assert_eq!(img.pixels[(SPEC_H - 1) * 2 + 1], bg);
        assert_eq!(img.pixels[0], bg);
    }

    #[test]
    fn sample_image_empty_take_is_single_bg_column() {
        let palette = build_palette(RAW_BASE_RGB);
        let img = sample_image(&[], &palette);
        assert_eq!(img.size, [1, SPEC_H]);
        assert!(img.pixels.iter().all(|&px| px == palette[0]));
    }

    fn test_app() -> (AbApp, Receiver<Command>) {
        let (cmd_tx, cmd_rx) = channel();
        let (frame_tx, frame_rx) = channel();
        // The frame sender stays alive for the app's lifetime in production;
        // these tests inject frames via handle_frame directly.
        std::mem::forget(frame_tx);
        (AbApp::new(cmd_tx, frame_rx), cmd_rx)
    }

    fn spectrum(ch: Channel) -> Frame {
        let mut bins = vec![DB_FLOOR; 64];
        bins[0] = 0.0;
        Frame::Spectrum { ch, bins }
    }

    #[test]
    fn recording_accumulates_sample_columns_live_does_not() {
        let (mut app, _cmd_rx) = test_app();
        // Boot view → Live: streaming alone never touches the pending take.
        app.send(Command::StartMonitor);
        app.handle_frame(&spectrum(Channel::Filtered));
        assert!(app.pending_bins[1].is_empty());
        // Recording: every non-empty column lands in the pending take (per
        // channel); empty spectra are skipped like the live ring skips them.
        app.send(Command::Record);
        app.handle_frame(&spectrum(Channel::Filtered));
        app.handle_frame(&spectrum(Channel::Raw));
        app.handle_frame(&Frame::Spectrum {
            ch: Channel::Filtered,
            bins: vec![],
        });
        assert_eq!(app.pending_bins[0].len(), 1);
        assert_eq!(app.pending_bins[1].len(), 1);
        assert!(app.sample_bins[0].is_empty());
        // RecordDone → Live: the take is promoted and stays frozen there.
        app.handle_frame(&Frame::RecordDone);
        app.handle_frame(&spectrum(Channel::Raw));
        assert_eq!(app.sample_bins[0].len(), 1);
        assert!(app.pending_bins[0].is_empty());
    }

    #[test]
    fn sample_capture_caps_at_max_columns() {
        let (mut app, _cmd_rx) = test_app();
        app.send(Command::StartMonitor);
        app.send(Command::Record);
        for _ in 0..SAMPLE_COLS_MAX + 5 {
            app.handle_frame(&spectrum(Channel::Raw));
        }
        assert_eq!(app.pending_bins[0].len(), SAMPLE_COLS_MAX);
    }

    #[test]
    fn take_promotes_on_record_done_only() {
        let (mut app, _cmd_rx) = test_app();
        app.send(Command::StartMonitor);
        app.send(Command::Record);
        app.handle_frame(&Frame::RecordStarted);
        app.handle_frame(&spectrum(Channel::Raw));
        app.handle_frame(&Frame::RecordDone);
        let generation = app.sample_gen;
        assert_eq!(app.sample_bins[0].len(), 1);
        // A re-take starts a fresh pending buffer but the reviewed sample
        // stays visible (and its texture cache valid) until RecordDone.
        app.send(Command::Record);
        assert!(app.pending_bins[0].is_empty());
        assert_eq!(app.sample_bins[0].len(), 1);
        assert_eq!(app.sample_gen, generation);
        // Cancelled re-take (backend confirms with RecordCancelled): the
        // reviewed sample survives untouched.
        app.handle_frame(&Frame::RecordStarted);
        app.handle_frame(&spectrum(Channel::Raw));
        app.send(Command::Stop);
        app.handle_frame(&Frame::RecordCancelled);
        assert_eq!(app.sample_bins[0].len(), 1);
        assert_eq!(app.sample_gen, generation);
        // A completed re-take replaces the sample and bumps the generation
        // — including the cancel-raced-completion case, where RecordDone
        // means the backend stored the take anyway.
        app.send(Command::Record);
        app.handle_frame(&Frame::RecordStarted);
        app.handle_frame(&spectrum(Channel::Raw));
        app.handle_frame(&spectrum(Channel::Raw));
        app.send(Command::Stop);
        app.handle_frame(&Frame::RecordDone);
        assert_eq!(app.sample_bins[0].len(), 2);
        assert_eq!(app.sample_gen, generation + 1);
    }

    #[test]
    fn record_started_clears_pretake_columns() {
        // Columns streamed between the optimistic Record and the backend's
        // RecordStarted are pre-take: the authoritative start re-clears.
        let (mut app, _cmd_rx) = test_app();
        app.send(Command::StartMonitor);
        app.send(Command::Record);
        app.handle_frame(&spectrum(Channel::Raw));
        assert_eq!(app.pending_bins[0].len(), 1);
        app.handle_frame(&Frame::RecordStarted);
        assert!(app.pending_bins[0].is_empty());
    }

    #[test]
    fn go_live_cooldown_arms_when_playback_yields_the_stop_slot() {
        let (mut app, _cmd_rx) = test_app();
        app.send(Command::StartMonitor);
        app.send(Command::Record);
        app.handle_frame(&Frame::RecordDone);
        // Natural end: PlaybackDone swaps Go live into the Stop slot.
        app.send(Command::Play(Channel::Raw));
        assert_eq!(app.go_live_cooldown, 0.0);
        app.handle_frame(&Frame::PlaybackDone);
        assert!(app.go_live_cooldown > 0.0);
        // User-initiated stop arms it too (second click of a double-click).
        app.go_live_cooldown = 0.0;
        app.send(Command::Play(Channel::Filtered));
        app.send(Command::Stop);
        assert!(app.go_live_cooldown > 0.0);
        // A stale PlaybackDone outside playback must not re-arm it.
        app.go_live_cooldown = 0.0;
        app.handle_frame(&Frame::PlaybackDone);
        assert_eq!(app.go_live_cooldown, 0.0);
    }

    #[test]
    fn refused_record_keeps_reviewed_take() {
        let (mut app, _cmd_rx) = test_app();
        app.send(Command::StartMonitor);
        app.send(Command::Record);
        app.handle_frame(&spectrum(Channel::Raw));
        app.handle_frame(&Frame::RecordDone);
        let generation = app.sample_gen;
        // Device gone: Record is refused by the state machine, so the take
        // under review must survive untouched.
        app.handle_frame(&Frame::Device {
            ok: false,
            name: String::new(),
        });
        app.send(Command::Record);
        assert_eq!(app.sample_bins[0].len(), 1);
        assert_eq!(app.sample_gen, generation);
    }

    #[test]
    fn device_recovery_autostarts_only_without_a_sample() {
        let (mut app, cmd_rx) = test_app();
        // Boot view (Sample, no sample): recovery resumes the split view.
        app.handle_frame(&Frame::Device {
            ok: true,
            name: "usb".into(),
        });
        assert_eq!(cmd_rx.try_recv(), Ok(Command::StartMonitor));
        assert!(matches!(app.state.mode, Mode::Live));
        // With a sample under review, recovery must not yank the user out.
        app.send(Command::Record);
        app.handle_frame(&Frame::RecordDone);
        app.handle_frame(&Frame::MonitorStopped);
        assert!(matches!(app.state.mode, Mode::Sample));
        while cmd_rx.try_recv().is_ok() {}
        app.handle_frame(&Frame::Device {
            ok: true,
            name: "usb".into(),
        });
        assert!(matches!(app.state.mode, Mode::Sample));
        assert!(cmd_rx.try_recv().is_err());
    }

    // kittest Tier-A harness: the window boots into the sample-less Sample
    // view, "Go live" is reachable by label, and clicking it emits
    // StartMonitor. Bounded stepping (not run()): the click flips the state
    // machine to Live, which legitimately keeps requesting repaints for the
    // animations.
    #[test]
    fn go_live_click_emits_start_monitor() {
        let (cmd_tx, cmd_rx) = channel();
        let (_frame_tx, frame_rx) = channel();
        let mut app = AbApp::new(cmd_tx, frame_rx);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
            .build(move |ctx| app.ui(ctx));
        harness.run_steps(2);
        harness.get_by_label("Go live").click();
        harness.run_steps(2);
        assert_eq!(cmd_rx.try_recv(), Ok(Command::StartMonitor));
    }

    // The window is fixed-size with no reserved bottom strip: the content
    // (probe: bottom of the last row) plus the 16 px frame margin must fit
    // the pinned height — and stay tight, so no dead strip creeps back in.
    // Measured in BOTH summary states (pre-sample hint / populated cards),
    // which card_frame pins to the same height so the row never jumps.
    #[test]
    fn window_fits_content_height() {
        let measure = |metrics: Option<SampleMetrics>| -> f32 {
            let (cmd_tx, _cmd_rx) = channel();
            let (_frame_tx, frame_rx) = channel();
            let mut app = AbApp::new(cmd_tx, frame_rx);
            if let Some(m) = metrics {
                app.handle_frame(&Frame::Metrics(m));
            }
            let mut harness = egui_kittest::Harness::builder()
                // Oversized on purpose: measure the content, don't clip it.
                .with_size(vec2(WINDOW_SIZE[0], 900.0))
                .build(move |ctx| app.ui(ctx));
            harness.run_steps(2);
            let bottom: f32 = harness
                .ctx
                .data(|d| d.get_temp(Id::new("t_content_bottom")))
                .expect("content probe must be set");
            bottom + 16.0 // bottom inner margin
        };
        let populated = SampleMetrics {
            voice_measurable: true,
            background_reduction_db: 34.0,
            raw_floor_dbfs: -42.3,
            filtered_floor_dbfs: -75.0,
            voice_retention_db: -0.8,
            raw_speech_dbfs: -18.0,
            filtered_speech_dbfs: -18.8,
        };
        // All THREE summary states must pin to the same height: the pre-sample
        // hint, populated cards, and the voice-not-measurable card (its "add
        // clear speech" branch renders more text and must not grow the row or
        // clip the fixed window).
        let hint = measure(None);
        let cards = measure(Some(populated));
        let na = measure(Some(SampleMetrics {
            voice_measurable: false,
            ..populated
        }));
        for (name, required) in [("hint", hint), ("cards", cards), ("not-measurable", na)] {
            assert!(
                (required - cards).abs() < 1.0,
                "summary row jumps in {name} state: {required} px vs cards {cards} px"
            );
            assert!(
                required <= WINDOW_SIZE[1],
                "{name}: content needs {required} px, window height is {}",
                WINDOW_SIZE[1]
            );
            assert!(
                WINDOW_SIZE[1] - required < 24.0,
                "dead strip in {name}: content ends at {required} px in a {} px window",
                WINDOW_SIZE[1]
            );
        }
    }

    #[test]
    fn frame_stream_updates_levels_and_spectrogram() {
        let (cmd_tx, _cmd_rx) = channel();
        let (frame_tx, frame_rx) = channel();
        let mut app = AbApp::new(cmd_tx, frame_rx);
        // The sample view decays meter targets to the floor; live frames
        // only hold in Live, so enter it like a real session would.
        app.state.on_command(Command::StartMonitor);
        frame_tx
            .send(Frame::Level {
                raw_db: -20.0,
                filtered_db: -40.0,
            })
            .unwrap();
        let mut bins = vec![DB_FLOOR; 64];
        bins[0] = 0.0;
        frame_tx
            .send(Frame::Spectrum {
                ch: Channel::Filtered,
                bins,
            })
            .unwrap();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
            .build(move |ctx| {
                app.ui(ctx);
                // Surface post-drain state for the assertions below.
                ctx.data_mut(|d| {
                    d.insert_temp(Id::new("t_lvl"), app.level_target);
                    d.insert_temp(Id::new("t_cols"), app.specs[1].columns.len());
                });
            });
        harness.run_steps(2);
        let lvl: [f32; 2] = harness.ctx.data(|d| d.get_temp(Id::new("t_lvl"))).unwrap();
        let cols: usize = harness.ctx.data(|d| d.get_temp(Id::new("t_cols"))).unwrap();
        assert_eq!(lvl, [-20.0, -40.0]);
        assert_eq!(cols, 1);
    }
}
