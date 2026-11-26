//! The widget gallery (dev only): every kit widget in every state and the
//! painters at fixed moments. Each sheet is a scene for the screenshot
//! harness ([`super::shot`]); the board replicas are in [`super::replica`].

use eframe::egui::{self, pos2, vec2, Context, Margin, Rect, Ui};

use super::fonts::Type;
use super::icons::Icon;
use super::paint::clips;
use super::paint::light::LightLayer;
use super::paint::live::{LiveLayout, LiveState, LiveView};
use super::paint::mark::{paint_logo, paint_mark, MarkState};
use super::paint::spectro::{self, Spectro};
use super::paint::take::{TakeMode, TakeState, TakeView};
use super::shot::{Scene, SceneArgs, SceneDef};
use super::text;
use super::widgets::{
    self, chip, figure, group, kbd, level_meter, link, note, pick, step_dots, tick, with_forced,
    Button, Force, MenuItem, NoteKind, Page, Rail, RailItem, Segmented, SelectItem, StepState,
    Strength, Switch,
};

pub fn scenes() -> Vec<SceneDef> {
    vec![
        SceneDef { name: "gallery/controls", about: "buttons, switches, segmented, strength, chips, links in every state", make: |a| Box::new(Controls::new(a)) },
        SceneDef { name: "gallery/surfaces", about: "figure, group, notes, banner, dropdown, menu, pick cards, meter, steps", make: |a| Box::new(Surfaces::new(a)) },
        SceneDef { name: "gallery/rail", about: "the rail, wide and compact, in each state", make: |a| Box::new(RailSheet::new(a)) },
        SceneDef { name: "gallery/marks", about: "the Mark in every state and size, and the morph", make: |_| Box::new(Marks) },
        SceneDef { name: "paint/live", about: "the Live picture, two lanes: state=on|ready|bypass|muted|off|starting|silent cols=N strength=DB replay=0..1 listen=0..1 w=", make: |a| Box::new(SpectroPic::new(a)) },
        SceneDef { name: "paint/path", about: "first run's picture, the lit path: state=on|light|ready|unfiltered|bypass|muted|off|starting cols=N moment=MS beam=0|1 w= h=", make: |a| Box::new(LivePic::new(a)) },
        SceneDef { name: "paint/take", about: "the Mic check timeline: mode=live|replay pos=0..1 listen=0..1 state=on|bypass|muted|off cols=N", make: |a| Box::new(TakePic::new(a)) },
    ]
}

fn page(ctx: &Context, add: impl FnOnce(&mut Ui)) {
    let pal = super::kit(ctx).pal;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(pal.bg)
                .inner_margin(Margin::same(24)),
        )
        .show(ctx, add);
}

fn heading(ui: &mut Ui, s: &str) {
    let pal = super::kit(ui.ctx()).pal;
    ui.add_space(4.0);
    text::label(ui, s, Type::EYEBROW, pal.ink_subtle);
    ui.add_space(8.0);
}

fn row(ui: &mut Ui, gap: f32, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = vec2(gap, 0.0);
        add(ui);
    });
    ui.add_space(14.0);
}

// ---------------------------------------------------------------- controls

struct Controls {
    on: bool,
    off: bool,
    model: usize,
    look: usize,
    strength: usize,
}

impl Controls {
    fn new(_: &SceneArgs) -> Controls {
        Controls {
            on: true,
            off: false,
            model: 0,
            look: 0,
            strength: 3,
        }
    }
}

impl Scene for Controls {
    fn ui(&mut self, ctx: &Context, _: &mut LightLayer) {
        page(ctx, |ui| {
            heading(ui, "Buttons · rest, hover, pressed, focus, disabled");
            for make in [
                Button::primary as fn(&'static str) -> Button<'static>,
                Button::secondary,
                Button::ghost,
            ] {
                row(ui, 12.0, |ui| {
                    ui.add(make("Turn on HushMic"));
                    with_forced(ui, Force::Hover, |ui| ui.add(make("Hover")));
                    with_forced(ui, Force::Press, |ui| ui.add(make("Pressed")));
                    with_forced(ui, Force::Focus, |ui| ui.add(make("Focus")));
                    ui.add(make("Disabled").enabled(false));
                    ui.add(make("Copy diagnostics").sm().icon(Icon::Copy));
                    ui.add(make("Mute").sm().icon(Icon::Mic));
                });
            }
            row(ui, 12.0, |ui| {
                ui.add(Button::hold("Hold to hear the original").icon(Icon::Hold));
                ui.add(
                    Button::hold("Hold to hear the original")
                        .icon(Icon::Hold)
                        .held(true),
                );
                ui.add(Button::primary("Replay the last 10 seconds").icon(Icon::Replay));
                ui.add(
                    Button::ghost("Hide get started")
                        .sm()
                        .icon_only(Icon::Close),
                );
            });
            row(ui, 12.0, |ui| {
                ui.add(
                    Button::hold("Hold to hear the original")
                        .icon(Icon::Hold)
                        .enabled(false),
                );
                ui.add(
                    Button::primary("Replay the last 10 seconds")
                        .icon(Icon::Replay)
                        .enabled(false),
                );
            });
            heading(ui, "Switch · off, on, focus, disabled");
            row(ui, 24.0, |ui| {
                ui.add(Switch::new(&mut self.off).name("Off"));
                ui.add(Switch::new(&mut self.on).name("On"));
                with_forced(ui, Force::Focus, |ui| {
                    ui.add(Switch::new(&mut self.on).name("Focus"))
                });
                ui.add(Switch::new(&mut self.off).name("Disabled").enabled(false));
                ui.add(Switch::new(&mut self.on).name("Disabled on").enabled(false));
                ui.add(Switch::new(&mut self.on).label("Noise suppression"));
                ui.add(Switch::new(&mut self.off).label("Noise suppression"));
            });
            heading(ui, "Segmented · two and three options, focus");
            row(ui, 24.0, |ui| {
                ui.allocate_ui(vec2(220.0, 38.0), |ui| {
                    Segmented::new("Model", &mut self.model, &[(0, "Quality"), (1, "Light")])
                        .width(220.0)
                        .show(ui)
                });
                ui.allocate_ui(vec2(240.0, 38.0), |ui| {
                    Segmented::new(
                        "Appearance",
                        &mut self.look,
                        &[(0, "System"), (1, "Dark"), (2, "Light")],
                    )
                    .width(240.0)
                    .show(ui)
                });
                ui.allocate_ui(vec2(200.0, 38.0), |ui| {
                    with_forced(ui, Force::Focus, |ui| {
                        let mut v = 1usize;
                        Segmented::new("Listen to", &mut v, &[(0, "Original"), (1, "HushMic")])
                            .width(200.0)
                            .id_salt("focus")
                            .show(ui)
                    })
                });
            });
            heading(ui, "Strength · one row, 2 × 2");
            row(ui, 24.0, |ui| {
                ui.allocate_ui(vec2(283.0, 38.0), |ui| {
                    Strength::new(
                        "Strength",
                        &mut self.strength,
                        &["Gentle", "Medium", "Strong", "Maximum"],
                    )
                    .show(ui)
                });
                ui.allocate_ui(vec2(220.0, 70.0), |ui| {
                    let mut v = 1;
                    Strength::new(
                        "Strength 2",
                        &mut v,
                        &["Sanft", "Mittel", "Stark", "Maximal"],
                    )
                    .two_by_two(true)
                    .show(ui)
                });
            });
            heading(ui, "Chips, keys, links");
            row(ui, 16.0, |ui| {
                chip(ui, "hushmic toggle mute", 30.0);
                kbd(ui, "Ctrl+Alt+M");
                kbd(ui, "Ctrl+Alt+Shift+Super+Space+Escape+PageDown");
                link(ui, "Choose another", false, false);
                with_forced(ui, Force::Hover, |ui| {
                    link(ui, "Hovered link", false, false)
                });
                link(ui, "Don\u{2019}t show again for Zoom", false, true);
                link(ui, "Website", true, false);
            });
        });
    }
}

// ---------------------------------------------------------------- surfaces

struct Surfaces {
    mic: usize,
    sw: bool,
    sw2: bool,
}

impl Surfaces {
    fn new(_: &SceneArgs) -> Surfaces {
        Surfaces {
            mic: 1,
            sw: true,
            sw2: false,
        }
    }
}

pub(super) const MICS: [SelectItem<'static>; 4] = [
    SelectItem {
        label: "System default",
        note: "Now: Built-in Audio",
    },
    SelectItem {
        label: "RODE NT-USB",
        note: "",
    },
    SelectItem {
        label: "Built-in Audio",
        note: "",
    },
    SelectItem {
        label: "USB Headset",
        note: "",
    },
];

impl Scene for Surfaces {
    fn ui(&mut self, ctx: &Context, _: &mut LightLayer) {
        let pal = super::kit(ctx).pal;
        page(ctx, |ui| {
            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                heading(ui, "Group of rows");
                group(ui, |g| {
                    g.row(|ui| {
                        ui.horizontal(|ui| {
                            text::label(ui, "Start HushMic when you log in", Type::LABEL, pal.ink);
                        });
                        ui.put(
                            Rect::from_min_size(
                                ui.max_rect().right_top() - vec2(44.0, 0.0),
                                vec2(44.0, 24.0),
                            ),
                            Switch::new(&mut self.sw).name("Start"),
                        );
                    });
                    g.row(|ui| {
                        text::label(ui, "Notifications", Type::LABEL, pal.ink);
                        ui.add_space(2.0);
                        text::label(
                            ui,
                            "When something needs you, and when your microphone comes back.",
                            Type::CAP,
                            pal.ink_subtle,
                        );
                        ui.put(
                            Rect::from_min_size(
                                ui.max_rect().right_top() - vec2(44.0, 0.0),
                                vec2(44.0, 24.0),
                            ),
                            Switch::new(&mut self.sw2).name("Notifications"),
                        );
                    });
                });
                ui.add_space(16.0);
                heading(ui, "Notes");
                note(
                    ui,
                    NoteKind::Warn,
                    "RODE NT-USB is unplugged.",
                    "Using Built-in Audio until it\u{2019}s back.",
                    None,
                );
                ui.add_space(8.0);
                note(
                    ui,
                    NoteKind::Info,
                    "Using Light for now.",
                    "Your computer is busy. HushMic goes back to Quality by itself.",
                    Some("Always use Light"),
                );
                ui.add_space(16.0);
                heading(ui, "Pick cards");
                pick(ui, true, "RODE NT-USB", "USB microphone");
                ui.add_space(8.0);
                pick(
                    ui,
                    false,
                    "Built-in Audio",
                    "Your computer\u{2019}s microphone",
                );

                let ui = &mut cols[1];
                heading(ui, "Figure");
                figure(ui, |ui| {
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        text::label(ui, "Input", Type::EYEBROW, pal.ink_subtle);
                    });
                    ui.add_space(60.0);
                });
                ui.add_space(16.0);
                heading(ui, "Dropdown, open");
                ui.allocate_ui(vec2(165.0, 34.0), |ui| {
                    with_forced(ui, Force::Open, |ui| {
                        widgets::select(ui, "Microphone", &mut self.mic, &MICS)
                    })
                });
                ui.add_space(170.0);
                heading(ui, "Level meter, steps, ticks");
                level_meter(ui, "Level", 0.62, 32, false);
                ui.add_space(12.0);
                step_dots(
                    ui,
                    "Step 2 of 3: Hear it work",
                    &[
                        ("Microphone", StepState::Done),
                        ("Hear it work", StepState::Current),
                        ("Use it in your apps", StepState::Ahead),
                    ],
                    false,
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(12.0, 0.0);
                    tick(ui, "Microphone picked", true);
                    tick(ui, "Call app set up", false);
                });
            });
        });
        // a menu, open
        let items = [
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
        widgets::select::menu_at(
            ctx,
            egui::Id::new("gallery-more"),
            pos2(720.0, 620.0),
            true,
            &items,
            None,
            widgets::select::ListKey::Nothing,
        );
    }
}

// ---------------------------------------------------------------- rail

struct RailSheet;

impl RailSheet {
    fn new(_: &SceneArgs) -> RailSheet {
        RailSheet
    }
}

impl Scene for RailSheet {
    fn ui(&mut self, ctx: &Context, _: &mut LightLayer) {
        let full = ctx.content_rect();
        let menu = [
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
        let states = [
            MarkState::Active,
            MarkState::Bypass,
            MarkState::Muted,
            MarkState::Off,
            MarkState::Error,
        ];
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ctx, |ui| {
                let mut x = full.left();
                for (i, m) in states.iter().enumerate() {
                    let compact = i >= 3;
                    let rw = Rail::width(compact);
                    let rect = Rect::from_min_size(pos2(x, full.top()), vec2(rw, full.height()));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(i));
                    let cur = [Page::Live, Page::Files, Page::Settings][i % 3];
                    Rail {
                        current: cur,
                        items: [
                            RailItem {
                                page: Page::Live,
                                label: "Live",
                            },
                            #[cfg(feature = "speakers")]
                            RailItem {
                                page: Page::Speakers,
                                label: "Speakers",
                            },
                            RailItem {
                                page: Page::Files,
                                label: "Files",
                            },
                            RailItem {
                                page: Page::Settings,
                                label: "Settings",
                            },
                        ],
                        mark: *m,
                        mark_name: "HushMic: On",
                        name: "Sections",
                        more: "More",
                        menu: &menu,
                        menu_about: &[],
                        compact,
                    }
                    .show(&mut child, rect);
                    x += rw + 8.0;
                }
            });
    }
}

// ---------------------------------------------------------------- marks

struct Marks;

impl Scene for Marks {
    fn ui(&mut self, ctx: &Context, _: &mut LightLayer) {
        let pal = super::kit(ctx).pal;
        page(ctx, |ui| {
            let p = ui.painter().clone();
            let o = ui.max_rect().min;
            let states = [
                MarkState::Active,
                MarkState::Bypass,
                MarkState::Off,
                MarkState::Muted,
                MarkState::Error,
            ];
            for (j, size) in [20.0_f32, 24.0, 48.0, 64.0].iter().enumerate() {
                for (i, s) in states.iter().enumerate() {
                    let at = o + vec2(i as f32 * 96.0, j as f32 * 84.0);
                    if *size <= 24.0 && j == 1 {
                        paint_mark(&p, at, *size, 1.0, *s, &pal, 1.0);
                    } else {
                        paint_logo(&p, at, *size, *s, &pal);
                    }
                }
            }
            // the morph at 64 px: 0 … 600 ms
            for (i, ms) in [0.0, 40.0, 80.0, 150.0, 250.0, 400.0, 600.0]
                .iter()
                .enumerate()
            {
                paint_mark(
                    &p,
                    o + vec2(i as f32 * 84.0, 4.0 * 84.0),
                    64.0,
                    ms / 600.0,
                    MarkState::Active,
                    &pal,
                    1.0,
                );
            }
        });
    }
}

// ---------------------------------------------------------------- the painters

pub(super) fn live_state(s: Option<&str>) -> LiveState {
    match s.unwrap_or("on") {
        "light" => LiveState::Light,
        "ready" => LiveState::Ready,
        "unfiltered" => LiveState::Unfiltered,
        "bypass" => LiveState::Bypass,
        "muted" => LiveState::Muted,
        "off" => LiveState::Off,
        "starting" => LiveState::Starting,
        _ => LiveState::On,
    }
}

/// A picture prefilled with the first `cols` frames of the keyboard clip
/// (the boards' `--ticks`), so it shows the same columns as the board.
pub fn demo_live(state: LiveState, cols: usize) -> LiveView {
    let mut v = LiveView::new(state, true);
    let kit = super::Kit::default();
    v.set_playing(0.0, true, &kit);
    v.prefill((0..cols).map(|i| clips::KEYBOARD.at(i)), 0.0);
    if matches!(state, LiveState::Starting) {
        v.set_stage(-10.0, 3);
    }
    v
}

struct LivePic {
    view: LiveView,
    size: egui::Vec2,
}

impl LivePic {
    fn new(a: &SceneArgs) -> LivePic {
        let state = live_state(a.get("state"));
        let mut view = demo_live(state, a.num("cols", 180.0) as usize);
        if let Some(ms) = a.get("moment").and_then(|v| v.parse().ok()) {
            view.scrub(Some(ms), a.flag("beam"));
        }
        LivePic {
            view,
            size: vec2(a.num("w", 710.0), a.num("h", 168.0)),
        }
    }
}

impl Scene for LivePic {
    fn ui(&mut self, ctx: &Context, light: &mut LightLayer) {
        let full = ctx.content_rect();
        let rect = Rect::from_center_size(full.center(), self.size);
        let p = ctx.layer_painter(egui::LayerId::background());
        self.view.layout = LiveLayout::default();
        self.view.paint(ctx, &p, rect, light);
    }
}

/// The Live picture on the made-up ten seconds
/// (`crate::miccheck::demo`), in one of its states.
struct SpectroPic {
    lanes: crate::miccheck::demo::Lanes,
    view: Spectro,
    show: spectro::Show,
    width: f32,
}

impl SpectroPic {
    fn new(a: &SceneArgs) -> SpectroPic {
        use crate::miccheck::types::Engine;
        let state = a.get("state").unwrap_or("on");
        let mode = match state {
            "bypass" => Engine::Bypass,
            "muted" => Engine::Muted,
            _ => Engine::Filtering,
        };
        let sound = !matches!(state, "off" | "starting" | "silent");
        let db = a.num("strength", 100.0);
        let mut lanes = crate::miccheck::demo::Lanes::new();
        if sound {
            for _ in 0..a.num("cols", 200.0) as usize {
                lanes.step(mode, db);
            }
        }
        let playing = a.get("replay").map(|_| {
            lanes.freeze(db);
            lanes.show_frozen(true);
            spectro::Playing {
                position: a.num("replay", 0.4),
                original: a.num("listen", 0.0),
            }
        });
        let lane = |glyph, label: &str, name: &str, strong, note: Option<(Option<Icon>, &str)>| {
            spectro::Lane {
                glyph,
                label: label.to_owned(),
                name: name.to_owned(),
                strong,
                select: None,
                tail: 0.0,
                sound: note.is_none(),
                note: note.map(|(i, w)| (i, w.to_owned())),
                shut: false,
            }
        };
        let mic_note = match state {
            "off" | "starting" => Some((None, "")),
            "silent" => Some((None, "No sound")),
            _ => None,
        };
        let apps_note = match state {
            "off" => Some((None, "")),
            "starting" => Some((None, "")),
            "silent" => Some((None, "")),
            _ => None,
        };
        let app = if state == "ready" {
            ("No app right now", false)
        } else {
            ("Zoom", true)
        };
        SpectroPic {
            lanes,
            view: Spectro::new(),
            show: spectro::Show {
                mic: lane(Icon::Mic, "Microphone", "RODE NT-USB", sound, mic_note),
                apps: if state == "muted" {
                    spectro::Lane {
                        sound: false,
                        shut: true,
                        ..lane(Icon::MicOff, "What apps hear", "Nothing, muted", true, None)
                    }
                } else {
                    lane(
                        Icon::AppGlyph,
                        "What apps hear",
                        app.0,
                        app.1 && sound,
                        apps_note,
                    )
                },
                playing,
                playing_label: "Playing".to_owned(),
                compact: a.compact(),
                lane: a.num("lane", if a.compact() { 76.0 } else { 160.0 }),
            },
            width: a.num("w", 798.0),
        }
    }
}

impl Scene for SpectroPic {
    fn ui(&mut self, ctx: &Context, _: &mut LightLayer) {
        let pal = super::kit(ctx).pal;
        let full = ctx.content_rect();
        let size = vec2(
            self.width.min(full.width() - 32.0),
            spectro::Sizes::of(self.show.compact, self.show.lane).height(),
        );
        let rect = Rect::from_center_size(full.center(), size);
        let p = ctx.layer_painter(egui::LayerId::background());
        p.rect_filled(full, 0.0, pal.bg);
        p.add(super::widgets::card_fill_shape(rect, &pal));
        self.view.sync(ctx, &self.lanes.picture(), 0.0);
        self.view.paint(ctx, &p, rect, &self.show);
    }
}

struct TakePic {
    take: TakeView,
}

impl TakePic {
    fn new(a: &SceneArgs) -> TakePic {
        let mut take = TakeView::new();
        take.state = match a.get("state").unwrap_or("on") {
            "bypass" => TakeState::Bypass,
            "muted" => TakeState::Muted,
            "off" => TakeState::Off,
            _ => TakeState::On,
        };
        if take.state != TakeState::Off {
            for i in 0..a.num("cols", 200.0) as usize {
                let (r, c) = clips::KEYBOARD.at(i);
                take.push(r, c);
            }
        }
        if a.get("mode") == Some("replay") {
            take.set_mode(TakeMode::Replay);
            take.position = a.num("pos", 0.0);
        }
        let listen = a.num("listen", 0.0);
        take.set_listen(0.0, listen > 0.5, true);
        TakePic { take }
    }
}

impl Scene for TakePic {
    fn ui(&mut self, ctx: &Context, _: &mut LightLayer) {
        let full = ctx.content_rect();
        let rect = Rect::from_center_size(full.center(), vec2(full.width() - 64.0, 196.0));
        let p = ctx.layer_painter(egui::LayerId::background());
        self.take.paint(ctx, &p, rect);
    }
}
