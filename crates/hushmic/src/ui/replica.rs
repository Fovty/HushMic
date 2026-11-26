//! Board replicas (dev only): app boards' states drawn with the kit at the
//! boards' geometry (measured with `board-shot.mjs --measure`), as
//! calibration sheets for widget parity. They are not the pages (pages build
//! those); parity cases in `scripts/ui-parity/scenes.json` compare them.

use eframe::egui::{self, pos2, vec2, Context, Painter, Pos2, Rect, Ui};

use super::fonts::Type;
use super::gallery::{demo_live, live_state, MICS};
use super::icons::Icon;
use super::paint::band;
use super::paint::clips;
use super::paint::keys::{self, KeyDiagram, Swatch};
use super::paint::light::LightLayer;
use super::paint::live::{LiveLayout, LiveState, LiveView, AFTER, FALL};
use super::paint::mark::MarkState;
use super::paint::take::TakeView;
use super::shot::{Scene, SceneArgs, SceneDef};
use super::text;
use super::widgets::{
    self, group, kbd, with_forced, Button, Force, MenuItem, Page, Rail, RailItem, Segmented,
    Strength, Switch,
};
use super::Palette;

pub fn scenes() -> Vec<SceneDef> {
    vec![
        SceneDef {
            name: "kit/main-replica",
            about: "App-Main, On · in a call (default and minimum size); live=STATE for another state's picture",
            make: |a| Box::new(MainReplica::new(a)),
        },
        SceneDef {
            name: "kit/firstrun-replica",
            about: "App-FirstRun, step 1: the microphone (default size)",
            make: |_| Box::new(FirstRunReplica),
        },
        SceneDef {
            name: "kit/mic-replica",
            about: "App-MicCheck, Listening (default size)",
            make: |a| Box::new(MicReplica::new(a)),
        },
        SceneDef {
            name: "kit/settings-replica",
            about: "App-Settings, the top of the page (default size)",
            make: |_| Box::new(SettingsReplica::default()),
        },
        SceneDef {
            name: "kit/shortcuts-replica",
            about: "App-Shortcuts, portal with three keys set (default size)",
            make: |_| Box::new(ShortcutsReplica),
        },
    ]
}

const MENU: [MenuItem<'static>; 2] = [
    MenuItem {
        label: "Turn off HushMic",
        icon: Some(Icon::Power),
        separator: false,
        hint: None,
    },
    MenuItem {
        label: "Quit HushMic",
        icon: Some(Icon::Quit),
        separator: true,
        hint: None,
    },
];

/// The rail with HushMic on; returns its width.
fn rail(ui: &mut Ui, full: Rect, current: Page, compact: bool) -> f32 {
    rail_with(ui, full, current, compact, MarkState::Active, false)
}

/// The rail with another status pill, and its More menu drawn open.
fn rail_with(
    ui: &mut Ui,
    full: Rect,
    current: Page,
    compact: bool,
    mark: MarkState,
    more_open: bool,
) -> f32 {
    let rw = Rail::width(compact);
    let rect = Rect::from_min_size(full.min, vec2(rw, full.height()));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt("rail"));
    let rail = Rail {
        current,
        items: [
            (Page::Live, "Live"),
            #[cfg(feature = "speakers")]
            (Page::Speakers, "Speakers"),
            (Page::Files, "Files"),
            (Page::Settings, "Settings"),
        ]
        .map(|(page, label)| RailItem { page, label }),
        mark,
        mark_name: "HushMic: On",
        name: "Sections",
        more: "More",
        menu: &MENU,
        menu_about: &[],
        compact,
    };
    if more_open {
        let _ = with_forced(&mut child, Force::Open, |ui| rail.show(ui, rect));
    } else {
        let _ = rail.show(&mut child, rect);
    }
    rw
}

/// `.hm-app-figure` at a fixed rect: the border and the frame light.
fn figure_frame(p: &Painter, fig: Rect, pal: &Palette) {
    p.rect_stroke(
        fig,
        12.0,
        egui::Stroke::new(1.0_f32, pal.line),
        egui::StrokeKind::Inside,
    );
    let (x0, x1) = (
        fig.left() + 0.12 * fig.width(),
        fig.right() - 0.12 * fig.width(),
    );
    let fl = pal.frame_light;
    p.add(egui::Shape::mesh(band(
        x0,
        x1,
        fig.top(),
        fig.top() + 1.0,
        2.0,
        |x| {
            let f = (x - x0) / (x1 - x0);
            let k = if f < 0.3 {
                f / 0.3
            } else if f > 0.7 {
                (1.0 - f) / 0.3
            } else {
                1.0
            };
            fl.gamma_multiply(k.clamp(0.0, 1.0))
        },
    )));
}

/// Text at a line box's top-left; returns the block's rect.
fn put(
    ctx: &Context,
    p: &Painter,
    s: &str,
    ty: Type,
    color: egui::Color32,
    at: Pos2,
    wrap: f32,
) -> Rect {
    text::layout(ctx, s, ty, color, wrap).paint(p, at)
}

// ---------------------------------------------------------------- App-Main

/// App-Main, "On · in a call", default and minimum size, drawn with the kit
/// at the board's geometry (from `board-shot.mjs --measure`). This is a
/// calibration sheet for the widgets, not the Live page (pages builds that).
struct MainReplica {
    view: LiveView,
    on: bool,
    dflt: bool,
    mic: usize,
    strength: usize,
    model: usize,
    compact: bool,
    /// App-Look-Beam's header: no reserved second line (the figure 20 px
    /// higher) and the controls at the heading's top.
    tight: bool,
    /// The rail's Mark for the `live` state, and more=1: its menu open.
    mark: MarkState,
    more: bool,
}

impl MainReplica {
    fn new(a: &SceneArgs) -> MainReplica {
        let compact = a.compact();
        // live=bypass|muted|light|unfiltered|ready|off: the picture of another
        // App-Main state (compare its region); sounding states play the clip
        let state = live_state(a.get("live"));
        // scrub=0..1 [beam=1]: the switch-on moment frozen as App-Look-Beam's
        // review scrub shows it (no sound: progress over fall + after)
        let scrub = a.get("scrub").and_then(|v| v.parse::<f32>().ok());
        let mut view = if let Some(p) = scrub {
            let beam = a.flag("beam");
            let mut v = LiveView::new(state, true);
            let span = if beam { FALL + AFTER } else { AFTER };
            v.scrub(Some(p.clamp(0.0, 1.0) * span), beam);
            v
        } else if matches!(
            state,
            LiveState::On
                | LiveState::Light
                | LiveState::Unfiltered
                | LiveState::Bypass
                | LiveState::Muted
        ) {
            demo_live(state, a.num("cols", 180.0) as usize)
        } else {
            LiveView::new(state, true)
        };
        view.layout = LiveLayout {
            columns: [1.0, 1.6, 1.0],
            gap: 0.0,
        };
        // compact Live names the mic under its end (the board's `labels`)
        view.mic_label = compact.then(|| "RODE NT-USB".to_owned());
        MainReplica {
            view,
            on: true,
            dflt: false,
            mic: 1,
            strength: 3,
            model: 0,
            compact,
            tight: a.get("head") == Some("tight"),
            mark: match a.get("live") {
                Some("muted") => MarkState::Muted,
                Some("bypass") => MarkState::Bypass,
                Some("off") => MarkState::Off,
                _ => MarkState::Active,
            },
            more: a.flag("more"),
        }
    }
}

impl Scene for MainReplica {
    fn ui(&mut self, ctx: &Context, light: &mut LightLayer) {
        let kit = super::kit(ctx);
        let pal: Palette = kit.pal;
        let full = ctx.content_rect();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(pal.bg)).show(ctx, |ui| {
            let rw = rail_with(ui, full, Page::Live, self.compact, self.mark, self.more);

            let (pad_x, pad_top) = if self.compact { (24.0, 20.0) } else { (32.0, 28.0) };
            let left = full.left() + rw + pad_x;
            let right = full.right() - pad_x;
            let top = full.top() + pad_top;
            let p = ui.painter().clone();
            // header: title (and a reserved second line), Mute + switch
            let title = text::layout(ctx, "Removing background noise.", Type::H, pal.ink, right - left);
            title.paint(&p, pos2(left, top));
            let sw_label = text::width(ctx, "Noise suppression", Type::SWITCH);
            let sw_w = 44.0 + 12.0 + sw_label;
            let (ctl_y, ctl_right) = if self.compact { (top + 28.0 + 4.0 + 20.0 + 12.0, left + 80.97 + 16.0 + sw_w) } else { (top, right) };
            // the switch centred on the Mute button (the board's sits 2.5 px
            // high: its 29 px inline box, an accident the design drops)
            let mute_dy = if self.tight { 0.0 } else { 4.0 };
            let sw_dy = mute_dy + 4.0;
            let sw_rect = Rect::from_min_size(pos2(ctl_right - sw_w, ctl_y + sw_dy), vec2(sw_w, 24.0));
            let mute = Button::secondary("Mute").sm().icon(Icon::Mic);
            let mute_w = mute.width(ctx);
            let mute_rect = Rect::from_min_size(pos2(sw_rect.left() - 16.0 - mute_w, ctl_y + mute_dy), vec2(mute_w, 32.0));
            ui.put(mute_rect, mute);
            ui.put(sw_rect, Switch::new(&mut self.on).label("Noise suppression"));

            // the figure: picture (168) over the chain (1 : 1.6 : 1)
            let fig_top = if self.compact {
                full.top() + 144.0
            } else if self.tight {
                full.top() + 80.0
            } else {
                full.top() + 100.0
            };
            let fig_w = right - left;
            let pic = Rect::from_min_size(pos2(left + 1.0, fig_top + 1.0), vec2(fig_w - 2.0, if self.compact { 150.0 } else { 168.0 }));
            let chain_top = pic.bottom() + 1.0;
            let unit = (fig_w - 2.0) / 3.6;
            let cols = [left + 1.0, left + 1.0 + unit, left + 1.0 + 2.6 * unit, right - 1.0];
            let pad = 16.0;
            let mut bottoms = [0.0_f32; 3];
            for (i, eyebrow) in ["Input", "Suppression", "Apps"].iter().enumerate() {
                // columns after the first have a 1 px border on their left
                let border = if i > 0 { 1.0 } else { 0.0 };
                let x = cols[i] + border + pad;
                let w = cols[i + 1] - cols[i] - border - 2.0 * pad;
                let mut y = chain_top + pad;
                text::layout(ctx, eyebrow, Type::EYEBROW, pal.ink_subtle, w).paint(&p, pos2(x, y));
                y += 16.0 + 10.0;
                match i {
                    0 => {
                        text::layout(ctx, "Microphone", Type::LABEL, pal.ink, w).paint(&p, pos2(x, y));
                        y += 20.0 + 10.0;
                        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x, y), vec2(w, 34.0))));
                        widgets::select(&mut c, "Microphone", &mut self.mic, &MICS);
                        y += 34.0 + 10.0;
                        let cap = text::layout(ctx, "Strength and model are remembered for each microphone.", Type::CAP, pal.ink_subtle, w);
                        bottoms[0] = cap.paint(&p, pos2(x, y)).bottom();
                    }
                    1 => {
                        text::layout(ctx, "Strength", Type::LABEL, pal.ink, w).paint(&p, pos2(x, y));
                        y += 20.0 + 10.0;
                        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x, y), vec2(w, 70.0))));
                        let r = Strength::new("Strength", &mut self.strength, &["Gentle", "Medium", "Strong", "Maximum"]).show(&mut c);
                        y += r.rect.height() + 10.0;
                        let cap = text::layout(ctx, "Removes as much noise as it can.", Type::CAP, pal.ink_subtle, w);
                        cap.paint(&p, pos2(x, y));
                        y += cap.size().y.max(36.0) + 10.0;
                        text::layout(ctx, "Model", Type::LABEL, pal.ink, w).paint(&p, pos2(x, y));
                        y += 20.0 + 10.0;
                        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x, y), vec2(w.min(220.0), 38.0))));
                        Segmented::new("Model", &mut self.model, &[(0, "Quality"), (1, "Light")]).width(w.min(220.0)).show(&mut c);
                        y += 38.0 + 10.0;
                        let cap = text::layout(ctx, "The best sound. HushMic steps down to Light by itself when your computer is busy.", Type::CAP, pal.ink_subtle, w);
                        bottoms[1] = cap.paint(&p, pos2(x, y)).bottom();
                    }
                    _ => {
                        let lw = w - 44.0 - 12.0;
                        let lb = text::layout(ctx, "Use as default microphone", Type::LABEL, pal.ink, lw);
                        let rh = lb.size().y.max(24.0);
                        lb.paint(&p, pos2(x, y + (rh - lb.size().y) / 2.0));
                        ui.put(Rect::from_min_size(pos2(x + w - 44.0, y + (rh - 24.0) / 2.0), vec2(44.0, 24.0)), Switch::new(&mut self.dflt).name("Use HushMic as the default microphone"));
                        y += rh + 10.0;
                        let cap = text::layout(ctx, "Apps that follow the system default then use HushMic without setup.", Type::CAP, pal.ink_subtle, w);
                        bottoms[2] = cap.paint(&p, pos2(x, y)).bottom();
                    }
                }
            }
            let fig_bottom = bottoms.iter().fold(0.0_f32, |m, b| m.max(*b)) + pad + 1.0;
            let fig = Rect::from_min_max(pos2(left, fig_top), pos2(right, fig_bottom));
            figure_frame(&p, fig, &pal);
            p.rect_filled(Rect::from_min_size(pos2(left + 1.0, chain_top - 1.0), vec2(fig_w - 2.0, 1.0)), 0.0, pal.line);
            for x in &cols[1..3] {
                let x = x.round();
                p.rect_filled(Rect::from_min_max(pos2(x, chain_top), pos2(x + 1.0, fig_bottom - 1.0)), 0.0, pal.line);
            }
            self.view.paint(ctx, &p, pic, light);
        });
    }
}

// ---------------------------------------------------------------- App-MicCheck

/// App-MicCheck, "Listening": the take (keyboard clip, 120 prefilled + 60
/// live columns, as the board after 60 ticks), its level row, the replay and
/// hold buttons, Listen.
struct MicReplica {
    take: TakeView,
    listen: usize,
    /// disabled=1: Replay and Hold as "Mic problem" shows them (compare the
    /// button row only)
    disabled: bool,
}

impl MicReplica {
    fn new(a: &SceneArgs) -> MicReplica {
        let mut take = TakeView::new();
        for i in 0..a.num("cols", 180.0) as usize {
            let (r, c) = clips::KEYBOARD.at(i);
            take.push(r, c);
        }
        MicReplica {
            take,
            listen: 1,
            disabled: a.flag("disabled"),
        }
    }
}

impl Scene for MicReplica {
    fn ui(&mut self, ctx: &Context, light: &mut LightLayer) {
        let pal = super::kit(ctx).pal;
        let full = ctx.content_rect();
        light.static_edge = true;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg))
            .show(ctx, |ui| {
                let rw = rail(ui, full, Page::Live, false);
                let left = full.left() + rw + 32.0;
                let right = full.right() - 32.0;
                let top = full.top() + 28.0;
                let w = right - left;
                let p = ui.painter().clone();
                put(ctx, &p, "Mic check", Type::EYEBROW, pal.ink_subtle, pos2(left, top), w);
                put(ctx, &p, "Say a sentence, then type or let the fan run.", Type::H, pal.ink, pos2(left, top + 20.0), w);
                put(
                    ctx,
                    &p,
                    "The last 10 seconds are kept while this page is open. Replay them, or hold Space to hear the original.",
                    Type::SUB,
                    pal.ink_muted,
                    pos2(left, top + 52.0),
                    520.0,
                );

                // the figure: the take (196) over the level row (43)
                let fig = Rect::from_min_size(pos2(left, top + 112.0), vec2(w, 1.0 + 196.0 + 43.0 + 1.0));
                let take = Rect::from_min_size(fig.min + vec2(1.0, 1.0), vec2(w - 2.0, 196.0));
                figure_frame(&p, fig, &pal);
                self.take.paint(ctx, &p, take);
                let t_y = take.bottom() - 10.0 - 16.0;
                put(ctx, &p, "10 s ago", Type::MONO, pal.ink_subtle, pos2(take.left() + 16.0, t_y), w);
                let now_w = text::width(ctx, "now", Type::MONO);
                put(ctx, &p, "now", Type::MONO, pal.ink_subtle, pos2(take.right() - 16.0 - now_w, t_y), w);
                p.rect_filled(Rect::from_min_size(pos2(take.left(), take.bottom()), vec2(take.width(), 1.0)), 0.0, pal.line);
                let row_y = take.bottom() + 1.0 + 12.0;
                let mut x = take.left() + 16.0;
                x += put(ctx, &p, "Level", Type::EYEBROW, pal.ink_subtle, pos2(x, row_y + 1.0), w).width() + 12.0;
                p.circle_filled(pos2(x + 4.0, row_y + 9.0), 4.0, pal.ink);
                let word = Type::CAP.medium().with_line(17.0);
                x += 8.0 + 6.0 + put(ctx, &p, "Sounds good", word, pal.ink, pos2(x + 14.0, row_y + 0.5), w).width() + 12.0;
                put(ctx, &p, "Your voice reaches into the light band.", Type::CAP, pal.ink_subtle, pos2(x, row_y), w);

                // replay, hold, "or hold Space"; Listen wraps to the next row
                let y = fig.bottom() + 20.0;
                let on = !self.disabled;
                let replay = Button::primary("Replay the last 10 seconds").icon(Icon::Replay).enabled(on);
                let rw1 = replay.width(ctx);
                ui.put(Rect::from_min_size(pos2(left, y), vec2(rw1, 40.0)), replay);
                let hold = Button::hold("Hold to hear the original").icon(Icon::Hold).enabled(on);
                let hx = left + rw1 + 16.0;
                let hw = hold.width(ctx);
                ui.put(Rect::from_min_size(pos2(hx, y), vec2(hw, 40.0)), hold);
                let ox = hx + hw + 16.0;
                let ow = put(ctx, &p, "or hold", Type::CAP, pal.ink_subtle, pos2(ox, y + 7.0 + 4.0), w).width();
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(ox + ow + 6.0, y + 7.0), vec2(120.0, 26.0))));
                kbd(&mut c, "Space");
                let y2 = y + 40.0 + 12.0;
                let lw = put(ctx, &p, "Listen", Type::LABEL, pal.ink, pos2(left, y2 + 9.0), w).width();
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(left + lw + 10.0, y2), vec2(200.0, 38.0))));
                Segmented::new("Listen to", &mut self.listen, &[(0, "Original"), (1, "HushMic")]).width(200.0).show(&mut c);
                put(ctx, &p, "Nothing is kept after you leave this page.", Type::CAP, pal.ink_subtle, pos2(left, y2 + 38.0 + 20.0), w);
            });
    }
}

// ---------------------------------------------------------------- App-Shortcuts

/// App-Shortcuts as the board opens: three keys set, push to mute not set.
struct ShortcutsReplica;

const SHORTCUTS: [(&str, &str, KeyDiagram, Option<&str>); 4] = [
    (
        "Toggle mute",
        "Press to mute, press again to unmute.",
        KeyDiagram::ToggleMute,
        Some("Ctrl+Alt+M"),
    ),
    (
        "Toggle noise suppression",
        "Press to send your raw microphone, press again to clean it.",
        KeyDiagram::ToggleBypass,
        Some("Ctrl+Alt+B"),
    ),
    (
        "Push to talk",
        "Your microphone is live only while you hold the key. Letting go mutes it.",
        KeyDiagram::PushToTalk,
        Some("Ctrl+Alt+Space"),
    ),
    (
        "Push to mute",
        "Silences your microphone while you hold the key, like a cough button.",
        KeyDiagram::PushToMute,
        None,
    ),
];

impl Scene for ShortcutsReplica {
    fn ui(&mut self, ctx: &Context, light: &mut LightLayer) {
        let pal = super::kit(ctx).pal;
        let full = ctx.content_rect();
        light.static_edge = true;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg))
            .show(ctx, |ui| {
                let rw = rail(ui, full, Page::Settings, false);
                let left = full.left() + rw + 32.0;
                let w = (full.right() - 32.0 - left).min(860.0);
                let top = full.top() + 28.0;
                let p = ui.painter().clone();

                // header: the words, and the button at its bottom right
                let button = Button::primary("Change shortcuts");
                let bw = button.width(ctx);
                let tw = w - bw - 24.0;
                put(ctx, &p, "Shortcuts", Type::EYEBROW, pal.ink_subtle, pos2(left, top), tw);
                let h = put(ctx, &p, "Control HushMic from any app with a key.", Type::H, pal.ink, pos2(left, top + 20.0), tw);
                let sub = put(
                    ctx,
                    &p,
                    "Your desktop keeps the keys. HushMic opens its shortcut window for you.",
                    Type::SUB,
                    pal.ink_muted,
                    pos2(left, h.bottom() + 4.0),
                    tw,
                );
                ui.put(Rect::from_min_size(pos2(left + w - bw, sub.bottom() - 40.0), vec2(bw, 40.0)), button);

                // the four shortcuts: words (1fr), the diagram, the key (150 px, at the end)
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                    pos2(left, sub.bottom() + 20.0),
                    vec2(w, 600.0),
                )));
                let g = group(&mut c, |g| {
                    for (title, desc, kind, key) in SHORTCUTS {
                        g.row(|ui| {
                            let cw = ui.available_width();
                            let words_w = cw - 20.0 - keys::SIZE.x - 20.0 - 150.0;
                            let cap = text::layout(ctx, desc, Type::CAP, pal.ink_subtle, words_w);
                            let rh = (20.0 + 2.0 + cap.size().y).max(keys::SIZE.y);
                            let (r, _) = ui.allocate_exact_size(vec2(cw, rh), egui::Sense::hover());
                            let p = ui.painter();
                            put(ctx, p, title, Type::LABEL, pal.ink, r.min, words_w);
                            cap.paint(p, r.min + vec2(0.0, 22.0));
                            let dx = r.left() + words_w + 20.0;
                            keys::diagram(p, pos2(dx, r.center().y - keys::SIZE.y / 2.0), kind, &pal);
                            match key {
                                Some(k) => {
                                    let kw = text::width(ctx, k, Type::MONO) + 18.0;
                                    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                                        pos2(r.right() - kw, r.center().y - 13.0),
                                        vec2(kw, 26.0),
                                    )));
                                    kbd(&mut c, k);
                                }
                                None => {
                                    let nw = text::width(ctx, "Not set", Type::CAP);
                                    put(ctx, p, "Not set", Type::CAP, pal.ink_subtle, pos2(r.right() - nw, r.center().y - 9.0), nw + 1.0);
                                }
                            }
                        });
                    }
                });

                // the legend, then the note
                let ly = g.response.rect.bottom() + 20.0;
                let mut x = left;
                for (s, label) in [
                    (Swatch::KeyHeld, "key held"),
                    (Swatch::Clean, "apps hear your cleaned voice"),
                    (Swatch::Raw, "raw microphone"),
                    (Swatch::Silence, "silence"),
                ] {
                    keys::swatch(&p, pos2(x, ly + (18.0 - keys::swatch_height(s)) / 2.0), s, &pal);
                    x += 22.0 + 6.0 + put(ctx, &p, label, Type::CAP, pal.ink_subtle, pos2(x + 28.0, ly), w).width() + 18.0;
                }
                put(
                    ctx,
                    &p,
                    "Mute only silences the HushMic microphone. Apps that use your microphone directly still hear you.",
                    Type::CAP,
                    pal.ink_subtle,
                    pos2(left, ly + 18.0 + 20.0),
                    w,
                );
            });
    }
}

// ---------------------------------------------------------------- App-Settings

/// App-Settings as it opens: General and Tray, the start of Microphones.
#[derive(Default)]
struct SettingsReplica {
    look: usize,
    tray_icon: usize,
}

/// A row's control on the right of its words.
#[derive(Clone, Copy)]
enum Control<'a> {
    /// The boards wrap the switch in a 29 px inline box (its baseline), so
    /// a row is at least 29 px tall; the switch is centred in it (the
    /// boards draw it at the box's top, 2.5 px high: an accident).
    Switch(&'a str),
    Button(&'a str),
    /// Name, which value (0 appearance, 1 tray icon), width.
    Seg(&'a str, usize, f32),
}

type Row<'a> = (&'a str, &'a str, Control<'a>);

const GENERAL: [Row<'static>; 4] = [
    (
        "Start HushMic when you log in",
        "",
        Control::Switch("Start HushMic when you log in"),
    ),
    (
        "Notifications",
        "When something needs you, and when your microphone comes back.",
        Control::Switch("Notifications"),
    ),
    (
        "Hidden notices",
        "OBS Studio: \u{201c}using your microphone directly\u{201d}.",
        Control::Button("Show again"),
    ),
    (
        "Appearance",
        "System follows your desktop\u{2019}s light or dark style.",
        Control::Seg("Appearance", 0, 240.0),
    ),
];

const TRAY: [Row<'static>; 2] = [
    ("Show tray icon", "Takes effect the next time HushMic starts.", Control::Switch("Show tray icon")),
    (
        "Tray icon",
        "Auto uses the monochrome icon on KDE and GNOME, which recolor it to match your panel, and the color icon elsewhere.",
        Control::Seg("Tray icon", 1, 300.0),
    ),
];

impl SettingsReplica {
    fn row(&mut self, ui: &mut Ui, (label, cap, control): Row<'_>) {
        let ctx = ui.ctx().clone();
        let pal = super::kit(&ctx).pal;
        let cw = ui.available_width();
        let (ctl_w, ctl_h) = match control {
            Control::Switch(_) => (44.0, 29.0),
            Control::Button(l) => (Button::secondary(l).sm().width(&ctx), 32.0),
            Control::Seg(_, _, sw) => (sw, 38.0),
        };
        let tw = cw - 12.0 - ctl_w;
        let lb = text::layout(&ctx, label, Type::LABEL, pal.ink, tw);
        let cb = (!cap.is_empty()).then(|| text::layout(&ctx, cap, Type::CAP, pal.ink_subtle, tw));
        let th = lb.size().y + cb.as_ref().map_or(0.0, |c| 2.0 + c.size().y);
        let rh = th.max(ctl_h);
        let (r, _) = ui.allocate_exact_size(vec2(cw, rh), egui::Sense::hover());
        let ty = r.center().y - th / 2.0;
        lb.paint(ui.painter(), pos2(r.left(), ty));
        if let Some(cb) = cb {
            cb.paint(ui.painter(), pos2(r.left(), ty + lb.size().y + 2.0));
        }
        let cr = Rect::from_min_size(
            pos2(r.right() - ctl_w, r.center().y - ctl_h / 2.0),
            vec2(ctl_w, ctl_h),
        );
        match control {
            Control::Switch(name) => {
                let mut on = true;
                ui.put(
                    Rect::from_center_size(cr.center(), vec2(44.0, 24.0)),
                    Switch::new(&mut on).name(name),
                );
            }
            Control::Button(l) => {
                ui.put(cr, Button::secondary(l).sm());
            }
            Control::Seg(name, which, sw) => {
                let looks = [(0, "System"), (1, "Dark"), (2, "Light")];
                let icons = [(0, "Auto"), (1, "Color"), (2, "Monochrome")];
                let (v, opts) = if which == 0 {
                    (&mut self.look, &looks)
                } else {
                    (&mut self.tray_icon, &icons)
                };
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(cr));
                Segmented::new(name, v, opts).width(sw).show(&mut c);
            }
        }
    }
}

impl Scene for SettingsReplica {
    fn ui(&mut self, ctx: &Context, light: &mut LightLayer) {
        let pal = super::kit(ctx).pal;
        let full = ctx.content_rect();
        light.static_edge = true;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg))
            .show(ctx, |ui| {
                let rw = rail(ui, full, Page::Settings, false);
                let left = full.left() + rw + 32.0;
                let w = (full.right() - 32.0 - left).min(760.0 - 64.0);
                let mut y = full.top() + 28.0;
                let p = ui.painter().clone();
                put(ctx, &p, "Settings", Type::H, pal.ink, pos2(left, y), w);
                y += 28.0 + 28.0;
                for (title, rows) in [("General", &GENERAL[..]), ("Tray", &TRAY[..])] {
                    put(
                        ctx,
                        &p,
                        title,
                        Type::EYEBROW,
                        pal.ink_subtle,
                        pos2(left, y),
                        w,
                    );
                    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                        pos2(left, y + 16.0 + 10.0),
                        vec2(w, 1000.0),
                    )));
                    let g = group(&mut c, |g| {
                        for row in rows {
                            g.row(|ui| self.row(ui, *row));
                        }
                    });
                    y = g.response.rect.bottom() + 28.0;
                }
                put(
                    ctx,
                    &p,
                    "Microphones",
                    Type::EYEBROW,
                    pal.ink_subtle,
                    pos2(left, y),
                    w,
                );
                put(
                    ctx,
                    &p,
                    "Each microphone keeps its own strength and model. The others share these.",
                    Type::CAP,
                    pal.ink_subtle,
                    pos2(left, y + 16.0 + 10.0 - 4.0),
                    w,
                );
            });
    }
}

// ---------------------------------------------------------------- App-FirstRun

/// App-FirstRun, step 1 (A · Three steps): the whole window, no rail.
struct FirstRunReplica;

const FR_MICS: [(&str, &str); 4] = [
    (
        "System default",
        "Follows your sound settings. Now: Built-in Audio",
    ),
    ("RODE NT-USB", "USB microphone"),
    ("Built-in Audio", "Your computer\u{2019}s microphone"),
    ("USB Headset", "Headset microphone"),
];

impl Scene for FirstRunReplica {
    fn ui(&mut self, ctx: &Context, _: &mut LightLayer) {
        let pal = super::kit(ctx).pal;
        let full = ctx.content_rect();
        // (no lit edge on steps 1 and 2)
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg))
            .show(ctx, |ui| {
                let p = ui.painter().clone();
                let child = |ui: &mut Ui, r: Rect| ui.new_child(egui::UiBuilder::new().max_rect(r));

                // the steps and Skip, 16 px from the top
                let skip = Button::ghost("Skip").sm();
                let sw = skip.width(ctx);
                ui.put(
                    Rect::from_min_size(
                        pos2(full.right() - 24.0 - sw, full.top() + 16.0),
                        vec2(sw, 32.0),
                    ),
                    skip,
                );
                let mut c = child(
                    ui,
                    Rect::from_min_size(full.min + vec2(24.0, 22.0), vec2(600.0, 20.0)),
                );
                widgets::step_dots(
                    &mut c,
                    "Step 1 of 3: Microphone",
                    &[
                        ("Microphone", widgets::StepState::Current),
                        ("Hear it work", widgets::StepState::Ahead),
                        ("Use it in your apps", widgets::StepState::Ahead),
                    ],
                    false,
                );

                // the body: 600 px, centred
                let left = full.center().x - 300.0;
                let mut y = full.top() + 80.0;
                let h = put(
                    ctx,
                    &p,
                    "Which microphone should HushMic clean?",
                    Type::H,
                    pal.ink,
                    pos2(left, y),
                    600.0,
                );
                let sub = put(
                    ctx,
                    &p,
                    "You can change it any time on Live.",
                    Type::SUB,
                    pal.ink_muted,
                    pos2(left, h.bottom() + 6.0),
                    600.0,
                );
                y = sub.bottom() + 20.0;
                for (i, (name, note)) in FR_MICS.iter().enumerate() {
                    let mut c = child(ui, Rect::from_min_size(pos2(left, y), vec2(600.0, 200.0)));
                    y = widgets::pick(&mut c, i == 0, name, note).rect.bottom() + 8.0;
                    if i == 0 {
                        // the level under the selected microphone: padding 2 16 8 44
                        let mut c = child(
                            ui,
                            Rect::from_min_size(
                                pos2(left + 44.0, y + 2.0),
                                vec2(600.0 - 60.0, 14.0),
                            ),
                        );
                        widgets::level_meter(&mut c, "Level", 0.62, 32, false);
                        put(
                            ctx,
                            &p,
                            "Speak: the bar moves when this microphone hears you.",
                            Type::CAP,
                            pal.ink_subtle,
                            pos2(left + 44.0, y + 2.0 + 14.0 + 6.0),
                            540.0,
                        );
                        y += 2.0 + 14.0 + 6.0 + 18.0 + 8.0 + 8.0;
                    }
                }
                y += 20.0 - 8.0;
                p.circle_filled(pos2(left + 3.0, y + 9.0), 3.0, pal.accent);
                put(
                    ctx,
                    &p,
                    "HushMic is ready in the background.",
                    Type::CAP,
                    pal.ink_subtle,
                    pos2(left + 14.0, y),
                    600.0,
                );

                // the footer: a hairline, Continue on the right
                let fy = full.bottom() - 73.0;
                p.rect_filled(
                    Rect::from_min_size(pos2(full.left(), fy), vec2(full.width(), 1.0)),
                    0.0,
                    pal.line,
                );
                let next = Button::primary("Continue");
                let nw = next.width(ctx);
                ui.put(
                    Rect::from_min_size(
                        pos2(full.right() - 24.0 - nw, fy + 1.0 + 16.0),
                        vec2(nw, 40.0),
                    ),
                    next,
                );
            });
    }
}
