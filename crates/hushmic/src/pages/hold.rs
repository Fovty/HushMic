//! The hold control (PLAN §1 hold rule), shared by Live's replay and Files:
//! one button, "Hold to hear the original", and page Space.
//!
//! [`Compare`] is the rule itself (pure). [`Hold`] wires it to egui: the
//! focused button's Enter and Space are its press (read before anything
//! is drawn, so the button never sees them as a click), the pointer's down
//! and up on it, the click an assistive technology sends with no press,
//! and page Space (a tap is the page's play, held [`HOLD_TAP`] it plays the
//! original). Key repeats are no presses, a release always counts wherever
//! the focus went meanwhile, and a press the window loses counts for
//! nothing.

use eframe::egui::{self, Key, Rect, Response, Ui};

use super::util;
use crate::ui::icons::Icon;
use crate::ui::widgets::Button;

/// A press of the compare control shorter than this is a tap (the
/// latch); longer, a hold. Page Space held this long holds too.
pub const HOLD_TAP: f64 = 0.18;

/// The hold rule (PLAN §1, shared with Files): any press plays the original
/// at once. Released within [`HOLD_TAP`] it is a tap and toggles the latch
/// (the original stays on; the next tap returns to HushMic); held longer it
/// is a hold, and its release returns to HushMic and clears the latch. A
/// click with no press at all (an assistive technology's action) toggles
/// the latch. Page Space: held [`HOLD_TAP`] it plays the original until
/// released and clears the latch; a tap is Replay/Stop (the caller's), so
/// a quick tap never flashes the raw take. What clears the latch is the
/// caller's (Live: a new take; Files: a new file).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Compare {
    /// The original plays.
    pub on: bool,
    pub latched: bool,
    /// The hold button is down: since when, and whether the latch was on.
    press: Option<(f64, bool)>,
    /// Page Space is down since; past [`HOLD_TAP`] it holds.
    space: Option<f64>,
    space_holds: bool,
}

impl Compare {
    pub fn press(&mut self, now: f64) {
        if self.press.is_none() {
            self.press = Some((now, self.latched));
            self.on = true;
        }
    }

    pub fn release(&mut self, now: f64) {
        if let Some((t0, was)) = self.press.take() {
            self.latched = now - t0 < HOLD_TAP && !was;
            self.on = self.latched || self.space_holds;
        }
    }

    pub fn pressed(&self) -> bool {
        self.press.is_some()
    }

    /// A press or page Space holds the original for now (the latch aside).
    pub fn held(&self) -> bool {
        self.press.is_some() || self.space_holds
    }

    /// A click with no press.
    pub fn click(&mut self) {
        self.latched = !self.latched;
        self.on = self.latched || self.space_holds;
    }

    pub fn space_is_down(&self) -> bool {
        self.space.is_some()
    }

    pub fn space_down(&mut self, now: f64) {
        if self.space.is_none() {
            self.space = Some(now);
        }
    }

    /// Page Space past [`HOLD_TAP`]: a hold.
    pub fn tick(&mut self, now: f64) {
        if let Some(t0) = self.space {
            if !self.space_holds && now - t0 >= HOLD_TAP {
                self.space_holds = true;
                self.latched = false;
                self.on = true;
            }
        }
    }

    /// How long until page Space becomes a hold (the caller wakes then).
    pub fn space_wait(&self, now: f64) -> Option<f64> {
        let t0 = self.space.filter(|_| !self.space_holds)?;
        Some((t0 + HOLD_TAP - now).max(0.0))
    }

    /// Page Space released: true when it was a tap (Replay/Stop).
    pub fn space_up(&mut self, now: f64) -> bool {
        self.tick(now);
        if self.space.take().is_none() {
            return false;
        }
        if std::mem::take(&mut self.space_holds) {
            self.latched = false;
            self.on = self.press.is_some();
            false
        } else {
            true
        }
    }

    /// The window lost the keyboard while Space was down: no tap.
    pub fn space_cancel(&mut self) {
        if self.space.take().is_some() && std::mem::take(&mut self.space_holds) {
            self.latched = false;
            self.on = self.press.is_some();
        }
    }

    /// The press ends without counting (the window lost the keyboard or
    /// the pointer): neither a tap nor a hold; the latch stays.
    pub fn cancel(&mut self) {
        self.press = None;
        self.on = self.latched || self.space_holds;
    }
}

/// How the button acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The hold rule: a press plays the original, a tap latches it.
    Hold,
    /// Each press switches sides, on its release (a replay's sequential
    /// take: the two sides can't switch at the same moment). Page Space is
    /// the page's play only, once per press however long it is held.
    Toggle,
}

/// The hold button and page Space, with their presses in flight.
#[derive(Debug, Default)]
pub struct Hold {
    pub compare: Compare,
    /// The button's id (its keys are read before it is drawn).
    id: Option<egui::Id>,
    /// Enter or Space pressed the focused button (its release ends the
    /// press), and whether the kind was Toggle then (only such a press
    /// toggles).
    key_press: Option<(Key, bool)>,
    /// The pointer was down on the button last frame.
    pointer_on: bool,
    /// The window lost the keyboard while the pointer held the button: that
    /// press is over; its release (and click) count for nothing.
    pointer_ignored: bool,
    /// [`Kind::Toggle`]: page Space is down (it acted on the press).
    space_once: bool,
    /// Space is not this control's now, on the page or on the button
    /// (Live: no replay plays, and Space never starts one); Enter on the
    /// button still is.
    pub space_off: bool,
}

impl Hold {
    /// Read the keys before anything is drawn. `active`: there is something
    /// to hold. Page Space is the page's while nothing has the focus, or
    /// the hold button or one of `keep` (Files: the time line and Play);
    /// any other focused control keeps its Space, as everywhere on the
    /// desktop (Save focused + Space = Save), and so does a dialog over
    /// `layer`. Returns true for a page Space tap: the page's play
    /// (Play/Pause, Replay/Stop).
    pub fn keys(
        &mut self,
        ctx: &egui::Context,
        layer: egui::LayerId,
        keep: &[egui::Id],
        active: bool,
        kind: Kind,
        now: f64,
    ) -> bool {
        let focused = ctx.memory(|m| m.focused());
        let page_keys = focused.is_none_or(|f| Some(f) == self.id || keep.contains(&f));
        let over = ctx.memory(|m| m.top_modal_layer().is_some_and(|l| l != layer));
        let window_focus = ctx.input(|i| i.focused);
        let toggle = kind == Kind::Toggle;
        if toggle && self.compare.pressed() {
            // the kind changed mid-press (Live: the replay a press
            // started froze as sequential): that press counts for nothing,
            // no release, no toggle; the pointer's release and click are
            // swallowed
            self.compare.cancel();
            self.key_press = None;
            if std::mem::take(&mut self.pointer_on) {
                self.pointer_ignored = true;
            }
        }
        let here = active && !over;
        let on_button = here && focused.is_some() && focused == self.id;
        let here = here && page_keys && !self.space_off;
        let presses = here && !toggle;
        let space_off = self.space_off;
        let mut tapped = false;
        let mut toggled = false;
        let (compare, key_press, space_once) =
            (&mut self.compare, &mut self.key_press, &mut self.space_once);
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                let egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    ..
                } = e
                else {
                    return true;
                };
                match (key, pressed) {
                    // the focused button: its press and release (a toggle
                    // switches once per press, on the release); key
                    // repeats are no presses
                    (Key::Space | Key::Enter, true)
                        if on_button && !(space_off && *key == Key::Space) =>
                    {
                        if !*repeat && key_press.is_none() && !compare.pressed() {
                            *key_press = Some((*key, toggle));
                            if !toggle {
                                compare.press(now);
                            }
                        }
                        false
                    }
                    (k, false) if key_press.is_some_and(|(p, _)| p == *k) => {
                        if let Some((_, true)) = key_press.take() {
                            toggled = true;
                        } else {
                            compare.release(now);
                        }
                        false
                    }
                    // a toggle: Space is the page's play, once per press
                    // however long it is held
                    (Key::Space, true) if here && toggle || *space_once => {
                        if !*repeat && !*space_once && here {
                            *space_once = true;
                            tapped = true;
                        }
                        false
                    }
                    (Key::Space, false) if *space_once => {
                        *space_once = false;
                        false
                    }
                    // page Space
                    (Key::Space, true) if presses || compare.space_is_down() => {
                        if !*repeat && presses {
                            compare.space_down(now);
                        }
                        false
                    }
                    (Key::Space, false) if compare.space_is_down() => {
                        tapped |= compare.space_up(now);
                        false
                    }
                    _ => true,
                }
            });
        });
        if !window_focus {
            if self.key_press.take().is_some() {
                self.compare.cancel();
            }
            self.compare.space_cancel();
            self.space_once = false;
        }
        if toggled {
            self.compare.click();
        }
        self.compare.tick(now);
        if let Some(wait) = self.compare.space_wait(now) {
            crate::ui::motion::repaint_in(ctx, std::time::Duration::from_secs_f64(wait));
        }
        tapped
    }

    /// The button as it looks: pressed while the original plays here or
    /// as the backend reports it (`heard`).
    pub fn look<'a>(&self, label: &'a str, enabled: bool, heard: bool) -> Button<'a> {
        let held = self.compare.on || heard;
        Button::hold(label)
            .icon(Icon::Hold)
            .held(held)
            .enabled(enabled)
            .pressed(held)
    }

    /// Draw the button and take its pointer and click: down is a press (the
    /// original at once), up its release; a click with no press at all (an
    /// assistive technology's action) toggles the latch, and so does any
    /// press or click of a [`Kind::Toggle`]. `active`: there is something
    /// to hold. Returns the response and whether a click latched the
    /// original (the caller starts its play, as a tap's press does).
    #[allow(clippy::too_many_arguments)]
    pub fn button(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        salt: &str,
        look: Button<'_>,
        desc: String,
        active: bool,
        kind: Kind,
        now: f64,
    ) -> (Response, bool) {
        let ctx = ui.ctx().clone();
        let resp = util::put(ui, r, salt, look);
        self.id = Some(resp.id);
        ctx.accesskit_node_builder(resp.id, |n| n.set_description(desc));
        if !active {
            self.key_press = None;
        }
        // egui stops calling a press "down on" the button once it can't be
        // a click (0.8 s, or a few pixels of travel): a press that began on
        // the button lasts until the button lets go, wherever it went
        let still_down = ctx.input(|i| i.pointer.any_down());
        if kind == Kind::Toggle {
            // a press or click picks the side (Enter and Space came as
            // presses already); not the release of a press a change of
            // kind cancelled
            let raw = resp.is_pointer_button_down_on() || self.pointer_ignored && still_down;
            let swallow = std::mem::replace(&mut self.pointer_ignored, false);
            self.pointer_ignored = swallow && raw;
            if active && resp.clicked() && !swallow {
                self.compare.click();
            }
            return (resp, false);
        }
        let raw = active
            && (resp.is_pointer_button_down_on()
                || (self.pointer_on || self.pointer_ignored) && still_down);
        if self.pointer_on && !ctx.input(|i| i.focused) {
            // the window lost the keyboard mid-press: that press is over,
            // its release counts for nothing
            self.compare.cancel();
            self.pointer_on = false;
            self.pointer_ignored = raw;
        }
        let swallow = std::mem::replace(&mut self.pointer_ignored, false);
        self.pointer_ignored = swallow && raw;
        let down = raw && !swallow;
        let mut latched = false;
        if down && !self.pointer_on {
            self.compare.press(now);
        } else if !down && self.pointer_on {
            self.compare.release(now);
        } else if !swallow && active && resp.clicked() && !self.compare.pressed() {
            self.compare.click();
            latched = self.compare.latched;
        }
        self.pointer_on = down;
        (resp, latched)
    }

    /// No press, page Space or key on the button; the latch stays.
    pub fn end_presses(&mut self) {
        self.compare.cancel();
        self.compare.space_cancel();
        self.pointer_on = false;
        self.pointer_ignored = false;
        self.space_once = false;
        self.key_press = None;
    }

    /// Nothing held or latched.
    pub fn clear(&mut self) {
        self.compare = Compare::default();
    }

    /// The hold ended elsewhere (the daemon let go): nothing held or
    /// latched, and the press under way counts for nothing, so a pointer
    /// or key still down does not start another hold until it is released
    /// and pressed again.
    #[cfg(feature = "speakers")]
    pub fn let_go(&mut self) {
        self.compare = Compare::default();
        if std::mem::take(&mut self.pointer_on) {
            self.pointer_ignored = true;
        }
        self.key_press = None;
        self.space_once = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hold_rule() {
        let mut c = Compare::default();
        // a tap latches the original; the next tap returns
        c.press(1.0);
        assert!(c.on);
        c.release(1.1);
        assert!(c.on && c.latched);
        c.press(2.0);
        c.release(2.05);
        assert!(!c.on && !c.latched);
        // a hold plays while held
        c.press(3.0);
        c.release(3.18);
        assert!(!c.on && !c.latched);
        // a hold while latched returns and clears the latch
        c.click();
        assert!(c.on && c.latched);
        c.press(4.0);
        assert!(c.on);
        c.release(4.5);
        assert!(!c.on && !c.latched);
        // a click with no press toggles
        c.click();
        c.click();
        assert!(!c.on && !c.latched);
    }

    #[test]
    fn page_space_is_a_tap_or_a_hold() {
        let mut c = Compare::default();
        c.space_down(1.0);
        assert!(c.space_wait(1.05).is_some_and(|w| (w - 0.13).abs() < 1e-9));
        assert!(c.space_up(1.1), "a tap: Replay or Stop");
        assert!(!c.on);
        // held 180 ms: the original, and the latch goes
        c.click();
        c.space_down(2.0);
        c.tick(2.1);
        assert!(c.latched);
        c.tick(2.18);
        assert!(c.on && !c.latched);
        assert_eq!(c.space_wait(2.2), None);
        assert!(!c.space_up(3.0));
        assert!(!c.on);
        // the window loses the keyboard: no tap
        c.space_down(4.0);
        c.space_cancel();
        assert!(!c.space_is_down() && !c.on);
    }
}
