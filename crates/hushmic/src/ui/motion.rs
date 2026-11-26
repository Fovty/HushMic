//! Easing, the display-rate moment clock, and the software-GL frame cap.
//!
//! Rules (constraints §4): nothing loops; a still window asks for no
//! frames; a moment asks for every display frame while it runs, then
//! nothing; live levels step at 20 Hz (10 Hz on a software renderer).

use std::time::Duration;

/// cubic-bezier(x1, y1, x2, y2) as a function of x (Newton, as the design
/// system's JS does it).
pub fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let a = |p1: f32, p2: f32| 1.0 - 3.0 * p2 + 3.0 * p1;
    let b = |p1: f32, p2: f32| 3.0 * p2 - 6.0 * p1;
    let c = |p1: f32| 3.0 * p1;
    let at = |t: f32, p1: f32, p2: f32| ((a(p1, p2) * t + b(p1, p2)) * t + c(p1)) * t;
    let slope = |t: f32, p1: f32, p2: f32| 3.0 * a(p1, p2) * t * t + 2.0 * b(p1, p2) * t + c(p1);
    let mut t = x;
    for _ in 0..8 {
        let s = slope(t, x1, x2);
        if s.abs() < 1e-6 {
            break;
        }
        t -= (at(t, x1, x2) - x) / s;
    }
    at(t.clamp(0.0, 1.0), y1, y2)
}

/// `ease-out`: cubic-bezier(0.16, 1, 0.3, 1). Everything that enters or
/// settles.
pub fn ease_out(x: f32) -> f32 {
    bezier(0.16, 1.0, 0.3, 1.0, x)
}

/// `ease-standard`: cubic-bezier(0.2, 0, 0, 1). State changes in place.
pub fn ease_standard(x: f32) -> f32 {
    bezier(0.2, 0.0, 0.0, 1.0, x)
}

pub fn c01(x: f32) -> f32 {
    if x > 0.0 {
        x.min(1.0)
    } else {
        0.0
    }
}

pub fn smooth(x: f32) -> f32 {
    let x = c01(x);
    x * x * (3.0 - 2.0 * x)
}

pub fn quad_in(x: f32) -> f32 {
    let x = c01(x);
    x * x
}

pub fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

/// The clock of a display-rate moment (the switch-on settle, the beam, the
/// Mark's morph). It starts on the first frame drawn and advances by the
/// frame time, clamped to 34 ms, so a late frame slows the moment for one
/// frame instead of skipping ahead (appeng §5.3).
#[derive(Clone, Copy, Debug, Default)]
pub struct MomentClock {
    t_ms: f32,
    last: Option<f64>,
}

impl MomentClock {
    pub const MAX_STEP_MS: f32 = 34.0;

    pub fn new() -> MomentClock {
        MomentClock::default()
    }

    /// A clock frozen at `t_ms` (review and screenshots).
    pub fn at(t_ms: f32) -> MomentClock {
        MomentClock { t_ms, last: None }
    }

    /// Advance to `now` (egui's input time, seconds) and return the
    /// moment's time in ms.
    pub fn tick(&mut self, now: f64) -> f32 {
        if let Some(last) = self.last {
            let dt = ((now - last) * 1000.0) as f32;
            self.t_ms += dt.clamp(0.0, Self::MAX_STEP_MS);
        }
        self.last = Some(now);
        self.t_ms
    }

    pub fn t_ms(&self) -> f32 {
        self.t_ms
    }
}

/// Ask for a frame `d` from now. egui takes its predicted frame time off
/// every `request_repaint_after` delay, and eframe leaves that at 1/60 s, so
/// a plain 50 ms request paints after 33 ms (a 20 Hz cadence ran at 30).
/// Every timed repaint in the window goes through here; any thread may call
/// it (a backend's wake-up, for one).
pub fn repaint_in(ctx: &eframe::egui::Context, d: Duration) {
    // egui takes it off only when it converts; mirror that so the two
    // always cancel
    let dt = ctx.input(|i| i.predicted_dt);
    let back = Duration::try_from_secs_f32(dt).unwrap_or(Duration::ZERO);
    ctx.request_repaint_after(d.saturating_add(back));
}

/// Ask for the frame at the next multiple of `period` on egui's clock: a
/// steady cadence that does not drift by each frame's own time.
pub fn repaint_on_beat(ctx: &eframe::egui::Context, period: Duration) {
    let now = ctx.input(|i| i.time);
    let p = period.as_secs_f64().max(1e-3);
    let mut next = ((now / p).floor() + 1.0) * p;
    // a frame a hair before a beat takes the one after (no frame twice)
    if next - now < 1e-3 {
        next += p;
    }
    if let Ok(d) = Duration::try_from_secs_f64(next - now) {
        repaint_in(ctx, d);
    }
}

/// When to ask for the next frame while a moment runs. A GPU paints every
/// display frame (vsync paces `request_repaint`); a software renderer caps
/// the moment near 30 fps on whole vblanks, `(n − 0.5) × period` with
/// n = round(Hz / 30), never a fixed millisecond threshold that would give
/// uneven frames above 60 Hz.
pub fn moment_repaint(ctx: &eframe::egui::Context) {
    let kit = super::kit(ctx);
    if kit.software {
        let period = kit.frame_period.max(1.0 / 400.0);
        let n = ((1.0 / period) / 30.0).round().max(1.0);
        repaint_in(ctx, Duration::from_secs_f32((n - 0.5) * period));
    } else {
        ctx.request_repaint();
    }
}

/// The live-level cadence: one column per 50 ms meter frame, two columns per
/// 100 ms paint on a software renderer.
pub fn live_period(ctx: &eframe::egui::Context) -> Duration {
    if super::kit(ctx).software {
        Duration::from_millis(100)
    } else {
        Duration::from_millis(50)
    }
}

/// The next live frame: on the [`live_period`] beat (20 Hz, 10 Hz on a
/// software renderer). Call it while audio moves, instead of
/// `request_repaint_after(live_period(ctx))`.
pub fn live_repaint(ctx: &eframe::egui::Context) {
    repaint_on_beat(ctx, live_period(ctx));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_ease_is_monotonic_and_pinned() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        let mut prev = 0.0;
        for i in 1..100 {
            let v = ease_out(i as f32 / 100.0);
            assert!(v >= prev);
            prev = v;
        }
        // ease-out front-loads: half way in time is well past half way
        assert!(ease_out(0.5) > 0.85);
    }

    /// The delays egui hands the windowing loop while `f` runs one frame
    /// at `time` (eframe's default predicted frame time, 1/60 s).
    fn delays(time: f64, mut f: impl FnMut(&eframe::egui::Context)) -> Vec<Duration> {
        use std::sync::{Arc, Mutex};
        let ctx = eframe::egui::Context::default();
        super::super::install(&ctx, super::super::Theme::Dark);
        let _ = ctx.run(Default::default(), |_| {});
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        ctx.set_request_repaint_callback(move |info| g.lock().unwrap().push(info.delay));
        let input = eframe::egui::RawInput {
            time: Some(time),
            predicted_dt: 1.0 / 60.0,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| f(ctx));
        let v = got.lock().unwrap().clone();
        v
    }

    fn near(v: &[Duration], ms: f64) -> bool {
        v.iter()
            .any(|d| (d.as_secs_f64() * 1000.0 - ms).abs() < 0.5)
    }

    #[test]
    fn timed_repaints_wait_as_long_as_asked() {
        // egui takes 1/60 s off every delay; the kit puts it back
        let v = delays(10.0, |ctx| repaint_in(ctx, Duration::from_millis(50)));
        assert!(near(&v, 50.0), "{v:?}");
        // the live beat: from 10.012 s the next 50 ms mark is 38 ms away
        let v = delays(10.012, live_repaint);
        assert!(near(&v, 38.0), "{v:?}");
        // a hair before a mark: the one after it
        let v = delays(10.0499, live_repaint);
        assert!(near(&v, 50.1), "{v:?}");
        // a delay too long to add to stays a long delay
        let v = delays(10.0, |ctx| repaint_in(ctx, Duration::MAX));
        assert!(v.iter().all(|d| *d > Duration::from_secs(3600)), "{v:?}");
        // a software renderer's moment: (2 − 0.5) vblanks at 60 Hz
        let v = delays(10.0, |ctx| {
            super::super::set_software(ctx, true);
            super::super::set_frame_period(ctx, 1.0 / 60.0);
            moment_repaint(ctx);
        });
        assert!(near(&v, 25.0), "{v:?}");
        // from another thread, between frames (a backend's wake-up)
        let ctx = eframe::egui::Context::default();
        let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let g = got.clone();
        ctx.set_request_repaint_callback(move |info| g.lock().unwrap().push(info.delay));
        let c = ctx.clone();
        std::thread::spawn(move || repaint_in(&c, Duration::from_millis(40)))
            .join()
            .unwrap();
        assert!(
            near(&got.lock().unwrap(), 40.0),
            "{:?}",
            got.lock().unwrap()
        );
    }

    #[test]
    fn a_late_frame_slows_the_moment_instead_of_skipping() {
        let mut c = MomentClock::new();
        assert_eq!(c.tick(10.0), 0.0);
        assert!((c.tick(10.016) - 16.0).abs() < 0.01);
        // a 200 ms hitch only advances the clock by 34 ms
        assert!((c.tick(10.216) - 50.0).abs() < 0.01);
    }
}
