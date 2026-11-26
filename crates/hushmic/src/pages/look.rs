//! Light or dark, and less motion: the daemon resolves Settings ›
//! Appearance (System / Dark / Light) against the desktop's style and
//! reads the desktop's reduced-motion setting (`AppState::desktop`); the
//! window draws what it says.

use eframe::egui;

use crate::appstate::{self, AppState};
use crate::ui::Theme;

#[derive(Default)]
pub struct Look {
    applied: Option<Theme>,
    motion: Option<bool>,
    /// Tests and screenshots pin the theme.
    pub pinned: Option<Theme>,
}

impl Look {
    /// The look the daemon spawned the window with (`--theme`,
    /// `--reduced-motion`), before the first frame: a light desktop never
    /// sees a dark flash. The snapshots rule after that.
    pub fn start(&mut self, ctx: &egui::Context, theme: appstate::Theme, reduced: bool) {
        let theme = self.pinned.unwrap_or_else(|| theme_of(theme));
        crate::ui::set_theme(ctx, theme);
        crate::ui::set_reduced_motion(ctx, reduced);
        self.applied = Some(theme);
        self.motion = Some(reduced);
    }

    pub fn apply(&mut self, ctx: &egui::Context, s: &AppState) {
        let theme = self
            .pinned
            .unwrap_or_else(|| theme_of(s.desktop.effective_theme));
        if self.applied != Some(theme) {
            crate::ui::set_theme(ctx, theme);
            self.applied = Some(theme);
        }
        let reduced = s.desktop.reduced_motion;
        if self.motion != Some(reduced) {
            crate::ui::set_reduced_motion(ctx, reduced);
            self.motion = Some(reduced);
        }
    }
}

fn theme_of(t: appstate::Theme) -> Theme {
    match t {
        appstate::Theme::Dark => Theme::Dark,
        appstate::Theme::Light => Theme::Light,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock;

    #[test]
    fn the_window_draws_what_the_desktop_look_says() {
        let ctx = egui::Context::default();
        crate::ui::install(&ctx, Theme::Dark);
        let mut look = Look::default();
        let mut s = mock::state(mock::Scenario::OnInCall);
        s.desktop.effective_theme = appstate::Theme::Light;
        s.desktop.reduced_motion = true;
        look.apply(&ctx, &s);
        assert!(!crate::ui::kit(&ctx).dark());
        assert!(crate::ui::kit(&ctx).reduced_motion);

        // The desktop changes its mind: the window follows.
        s.desktop.effective_theme = appstate::Theme::Dark;
        s.desktop.reduced_motion = false;
        look.apply(&ctx, &s);
        assert!(crate::ui::kit(&ctx).dark());
        assert!(!crate::ui::kit(&ctx).reduced_motion);

        // A pinned theme wins (screenshots); motion still follows.
        look.pinned = Some(Theme::Light);
        s.desktop.reduced_motion = true;
        look.apply(&ctx, &s);
        assert!(!crate::ui::kit(&ctx).dark());
        assert!(crate::ui::kit(&ctx).reduced_motion);
    }

    #[test]
    fn the_spawn_look_is_drawn_before_any_snapshot() {
        let ctx = egui::Context::default();
        crate::ui::install(&ctx, Theme::Dark);
        let mut look = Look::default();
        look.start(&ctx, appstate::Theme::Light, true);
        assert!(!crate::ui::kit(&ctx).dark());
        assert!(crate::ui::kit(&ctx).reduced_motion);

        // The first snapshot says the same: nothing changes. Then it rules.
        let mut s = mock::state(mock::Scenario::OnInCall);
        s.desktop.effective_theme = appstate::Theme::Light;
        s.desktop.reduced_motion = true;
        look.apply(&ctx, &s);
        assert!(!crate::ui::kit(&ctx).dark());
        s.desktop.effective_theme = appstate::Theme::Dark;
        s.desktop.reduced_motion = false;
        look.apply(&ctx, &s);
        assert!(crate::ui::kit(&ctx).dark());
        assert!(!crate::ui::kit(&ctx).reduced_motion);
    }
}
