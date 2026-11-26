//! Launch B: the window opens at once while HushMic starts, dark, with the
//! Mark standing (its noise bars up). "Starting…" appears under it after
//! 0.7 s if the start takes that long. When the start ends, the Mark
//! settles into the line (the same morph as the beam's impact, only when
//! the start ends in filtering) and the page fades in underneath, usable
//! at once. Skipped when HushMic is already running when the window opens.

use eframe::egui::{self, pos2, vec2, LayerId, Order, Rect};

use crate::appstate::{AppState, Phase, StartReason};
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::motion::{c01, ease_out};
use crate::ui::paint::mark::{paint_mark, MarkMorph, MarkState};
use crate::ui::text;

const MARK: f32 = 64.0;
const GAP: f32 = 18.0;
/// "Starting…" shows when the start takes longer than this.
const NOTE_AFTER: f64 = 0.7;
const NOTE_FADE: f64 = 0.22;
/// After the start: the layer stays while the Mark settles, then fades.
const FADE_DELAY: f64 = 0.4;
const FADE: f64 = 0.32;

#[derive(Default)]
pub struct Launch {
    decided: bool,
    phase: Stage,
}

#[derive(Default)]
enum Stage {
    #[default]
    None,
    Wait {
        t0: f64,
    },
    Settle {
        t: f64,
        morph: MarkMorph,
        filtering: bool,
        /// "Starting…"'s opacity when the start ended: it fades out from
        /// there.
        note: f32,
    },
}

/// "Starting…"'s opacity `since` the layer opened (fading in with the
/// brand ease-out after 0.7 s).
fn note_in(since: f64) -> f32 {
    ease_out(c01(((since - NOTE_AFTER) / NOTE_FADE) as f32))
}

impl Launch {
    /// The window's first frame with a state: does the launch layer play?
    pub fn open(&mut self, s: &AppState, now: f64, first_run: bool) {
        if self.decided {
            return;
        }
        self.decided = true;
        let launching =
            s.engine.phase == Phase::Starting && s.engine.start_reason == Some(StartReason::Launch);
        if launching && !first_run {
            self.phase = Stage::Wait { t0: now };
        }
    }

    pub fn skip(&mut self) {
        self.decided = true;
        self.phase = Stage::None;
    }

    /// The layer hides the pages (nothing to draw under it yet).
    pub fn covers(&self) -> bool {
        matches!(self.phase, Stage::Wait { .. })
    }

    /// Once per frame, after the pages. `reduced`: no morph, no fade.
    pub fn ui(&mut self, ctx: &egui::Context, s: &AppState, now: f64, full: Rect) {
        let kit = crate::ui::kit(ctx);
        let reduced = kit.reduced_motion;
        if let Stage::Wait { t0 } = self.phase {
            if s.engine.phase != Phase::Starting {
                let filtering = super::status::filtering(s);
                let mut morph = MarkMorph::new(false);
                morph.set(now, filtering, reduced);
                self.phase = if reduced {
                    Stage::None
                } else {
                    Stage::Settle {
                        t: now,
                        morph,
                        filtering,
                        note: note_in(now - t0),
                    }
                };
            }
        }
        let (t, alpha, note) = match &mut self.phase {
            Stage::None => return,
            Stage::Wait { t0 } => {
                let since = now - *t0;
                let note = note_in(since);
                if since < NOTE_AFTER {
                    crate::ui::motion::repaint_in(
                        ctx,
                        std::time::Duration::from_secs_f64(NOTE_AFTER - since),
                    );
                } else if note < 1.0 {
                    ctx.request_repaint();
                }
                (0.0, 1.0, note)
            }
            Stage::Settle {
                t,
                morph,
                filtering,
                note,
            } => {
                let since = now - *t;
                let m = if *filtering { morph.tick(now) } else { 0.0 };
                // "Starting…" fades out as it faded in (220 ms)
                let note = *note * (1.0 - ease_out(c01((since / NOTE_FADE) as f32)));
                let fade = c01(((since - FADE_DELAY) / FADE) as f32);
                let alpha = 1.0 - ease_out(fade);
                if since >= FADE_DELAY + FADE {
                    self.phase = Stage::None;
                    return;
                }
                crate::ui::motion::moment_repaint(ctx);
                (m, alpha, note)
            }
        };
        let pal = kit.pal;
        let p = ctx.layer_painter(LayerId::new(Order::Foreground, egui::Id::new("hm-launch")));
        p.rect_filled(full, 0.0, pal.bg.gamma_multiply(alpha));
        let starting = tr!("launch-starting");
        let nb = text::layout(
            ctx,
            &starting,
            Type::CAP,
            pal.ink_subtle.gamma_multiply(alpha * note),
            f32::INFINITY,
        );
        let h = MARK + GAP + Type::CAP.line;
        let top = full.center().y - h / 2.0;
        let mx = full.center().x - MARK / 2.0;
        let state = if t > 0.0 {
            MarkState::Active
        } else {
            MarkState::Bypass
        };
        paint_mark(&p, pos2(mx, top), MARK, t, state, &pal, alpha);
        nb.paint(
            &p,
            pos2(full.center().x - nb.size().x / 2.0, top + MARK + GAP),
        );
        if self.covers() {
            // the status for screen readers while nothing else shows
            let r = Rect::from_center_size(full.center(), vec2(MARK, MARK));
            // not interactable: the label only, no Tab stop of its own
            egui::Area::new(egui::Id::new("hm-launch-status"))
                .fixed_pos(r.min)
                .order(Order::Foreground)
                .interactable(false)
                .show(ctx, |ui| {
                    let (_, resp) = ui.allocate_exact_size(r.size(), egui::Sense::hover());
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &starting)
                    });
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{state, Scenario};

    #[test]
    fn plays_only_while_a_launch_start_is_under_way() {
        let mut l = Launch::default();
        l.open(&state(Scenario::Starting), 0.0, false);
        assert!(l.covers());
        let mut l = Launch::default();
        l.open(&state(Scenario::OnInCall), 0.0, false);
        assert!(!l.covers());
        // first run shows its own engine line instead
        let mut s = state(Scenario::FirstRun1);
        s.engine.phase = Phase::Starting;
        s.engine.start_reason = Some(StartReason::Launch);
        let mut l = Launch::default();
        l.open(&s, 0.0, true);
        assert!(!l.covers());
        // a turn-on from Off is not a launch
        let mut s = state(Scenario::Starting);
        s.engine.start_reason = Some(StartReason::TurnOn);
        let mut l = Launch::default();
        l.open(&s, 0.0, false);
        assert!(!l.covers());
    }

    #[test]
    fn the_layer_lifts_once_the_start_ends() {
        let ctx = egui::Context::default();
        crate::ui::install(&ctx, crate::ui::Theme::Dark);
        let mut l = Launch::default();
        l.open(&state(Scenario::Starting), 0.0, false);
        let full = Rect::from_min_size(pos2(0.0, 0.0), vec2(960.0, 640.0));
        let ready = state(Scenario::OnInCall);
        for (i, t) in [0.1, 0.2, 1.0, 1.4, 2.0].iter().enumerate() {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                let s = if i == 0 {
                    state(Scenario::Starting)
                } else {
                    ready.clone()
                };
                l.ui(ctx, &s, *t, full);
            });
        }
        assert!(!l.covers());
        assert!(matches!(l.phase, Stage::None));
    }

    #[test]
    fn reduced_motion_lifts_the_layer_at_once() {
        let ctx = egui::Context::default();
        crate::ui::install(&ctx, crate::ui::Theme::Dark);
        crate::ui::set_reduced_motion(&ctx, true);
        let mut l = Launch::default();
        l.open(&state(Scenario::Starting), 0.0, false);
        let full = Rect::from_min_size(pos2(0.0, 0.0), vec2(960.0, 640.0));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            l.ui(ctx, &state(Scenario::OnInCall), 0.5, full);
        });
        assert!(matches!(l.phase, Stage::None));
    }

    #[test]
    fn the_cover_reads_starting_and_takes_no_tab_stop() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut s = state(Scenario::Starting);
        s.engine.start_reason = Some(StartReason::Launch);
        let mut w =
            crate::pages::testing::window(s, crate::appstate::Page::Live, egui::vec2(960.0, 640.0));
        assert!(w.harness.query_by_label("Starting…").is_some());
        w.harness.key_press(egui::Key::Tab);
        w.harness.run_steps(2);
        let focused = w
            .harness
            .query_by(|n| n.is_focused())
            .map(|n| format!("{:?}", n.accesskit_node().role()));
        assert_eq!(focused, None);
    }
}
