//! Small things the pages share: placing blocks at the boards' coordinates,
//! Copy diagnostics, and opening links in the desktop's browser.

use std::hash::Hash;

use eframe::egui::{self, pos2, vec2, Pos2, Rect, Response, Ui, UiBuilder, Widget};

use crate::ui::text::{self, TextBlock};

/// The project's pages the window links to.
pub mod url {
    pub const WEBSITE: &str = "https://hushmic.org";
    pub const ISSUES: &str = "https://github.com/Fovty/hushmic/issues";
    pub const TROUBLESHOOTING: &str =
        "https://github.com/Fovty/hushmic/blob/main/docs/troubleshooting.md";
    pub const USAGE: &str = "https://github.com/Fovty/hushmic/blob/main/docs/usage.md";
    pub const HEADLESS: &str = "https://github.com/Fovty/hushmic/blob/main/docs/headless.md";
    pub const HOLD_RECIPE: &str = "https://github.com/Fovty/hushmic/blob/main/docs/headless.md\
                                   #push-to-talk-in-sway-i3-and-other-window-managers";
}

/// A child `Ui` over `rect` with a stable id (controls keep their focus and
/// hover state when the page's other contents change). egui's own child
/// ids count the children drawn before it, so a line that comes and goes
/// above a control (a header's second line) would re-id the control and
/// drop its focus; this id is the parent's and `salt` only, which must be
/// unique within the parent.
pub fn child(ui: &mut Ui, rect: Rect, salt: impl Hash) -> Ui {
    let id = ui.id().with(salt);
    ui.new_child(UiBuilder::new().max_rect(rect).id(id))
}

/// `r` takes the focus that `from` had, a control the state replaced
/// (Turn on by the switch), unless the focus moved on meanwhile. egui lets
/// go of a vanished widget's focus only after the pass, so `from` may
/// still hold it.
pub fn take_focus(r: &Response, from: egui::Id) {
    let f = r.ctx.memory(|m| m.focused());
    if f.is_none() || f == Some(from) {
        r.request_focus();
    }
}

/// Names as the window shows them: the second and later of a repeated
/// name numbered ("Zoom", "Zoom (2)"), so two apps of one name (a Flatpak
/// and a package) stay apart on screen and for screen readers.
/// Microphones come numbered in the snapshot already (the tray names them
/// the same way).
pub fn numbered<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    names
        .into_iter()
        .map(|name| {
            let k = seen.entry(name).or_insert(0);
            *k += 1;
            if *k == 1 {
                name.to_owned()
            } else {
                crate::tr!("name-repeat", name = name.to_owned(), n = k.to_string())
            }
        })
        .collect()
}

/// Place a widget in `rect`.
pub fn put(ui: &mut Ui, rect: Rect, salt: impl Hash, widget: impl Widget) -> Response {
    let mut c = child(ui, rect, salt);
    c.add(widget)
}

/// Show a laid-out text block with its line boxes' top-left at `at` (a
/// label in the accessibility tree). Returns its rect.
pub fn text_at(ui: &mut Ui, block: TextBlock, at: Pos2) -> Rect {
    let rect = Rect::from_min_size(at, block.size());
    let mut c = ui.new_child(UiBuilder::new().max_rect(rect));
    text::show(&mut c, block);
    rect
}

/// What a text block is to screen readers beyond its words (UX panel B16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speak {
    /// A heading at this level: screen readers jump by headings.
    Heading(usize),
    /// A heading that is also a polite live region: read out when it
    /// appears (a new page or step) or its words change (Live's state).
    LiveHeading(usize),
    /// A polite live region (a status line).
    Live,
}

/// [`text_at`] for a heading or a live region.
pub fn text_as(ui: &mut Ui, block: TextBlock, at: Pos2, speak: Speak) -> Rect {
    let rect = Rect::from_min_size(at, block.size());
    let mut c = ui.new_child(UiBuilder::new().max_rect(rect));
    let r = text::show(&mut c, block);
    tell(ui.ctx(), r.id, speak);
    rect
}

/// Give the node `id` (a label) its heading role or live politeness.
pub fn tell(ctx: &egui::Context, id: egui::Id, speak: Speak) {
    use egui::accesskit::{Live, Role};
    // a heading is named by its label: egui gives a Label's words as its
    // value, which only the Label role reads as its name
    let heading = |n: &mut egui::accesskit::Node, level: usize| {
        if let Some(words) = n.value().map(str::to_owned) {
            n.clear_value();
            n.set_label(words);
        }
        n.set_role(Role::Heading);
        n.set_level(level);
    };
    crate::ui::widgets::a11y(ctx, id, |n| match speak {
        Speak::Heading(level) => heading(n, level),
        Speak::LiveHeading(level) => {
            heading(n, level);
            n.set_live(Live::Polite);
        }
        Speak::Live => n.set_live(Live::Polite),
    });
}

/// A control's tooltip, which screen readers get as its description.
pub fn tip(r: Response, text: &str) -> Response {
    r.ctx
        .accesskit_node_builder(r.id, |n| n.set_description(text));
    r.on_hover_text(text)
}

/// Paint a block with no node of its own: a control's visible label (the
/// control carries the same words as its name).
pub fn paint_at(ui: &Ui, block: TextBlock, at: Pos2) -> Rect {
    block.paint(ui.painter(), at)
}

/// A link as one item of a wrapping row (`flex-wrap`): it moves to the
/// next line whole instead of breaking its words.
pub fn row_link(ui: &mut Ui, label: &str, external: bool, quiet: bool) -> Response {
    let natural = text::layout(
        ui.ctx(),
        label,
        crate::ui::fonts::Type::LINK,
        egui::Color32::WHITE,
        f32::INFINITY,
    )
    .size()
    .x + if external { 20.0 } else { 0.0 };
    if natural > ui.available_width() + 0.5 && ui.cursor().min.x > ui.max_rect().left() + 0.5 {
        ui.end_row();
    }
    crate::ui::widgets::link(ui, label, external, quiet)
}

/// Wakes the window from a backend thread (Mic check, the level probe). On
/// a software renderer no sooner than the live cadence allows (the
/// software-GL guard, PLAN §1.4): the backend asks after every column.
pub fn wake(ctx: &egui::Context) -> crate::miccheck::Repaint {
    let ctx = ctx.clone();
    std::sync::Arc::new(move |d| {
        let d = if crate::ui::kit(&ctx).software {
            d.max(crate::ui::motion::live_period(&ctx))
        } else {
            d
        };
        crate::ui::motion::repaint_in(&ctx, d)
    })
}

/// The page column (round 3, one rule for every page): at most
/// [`COLUMN_MAX`] px of content, centred in `area` (the space right of the
/// rail), with at least 32 px at the sides (24 px in the compact window).
/// Returns its left edge and width.
pub fn column(area: Rect, compact: bool) -> (f32, f32) {
    let side = if compact { 24.0 } else { 32.0 };
    let w = (area.width() - 2.0 * side).clamp(0.0, COLUMN_MAX);
    (area.left() + (area.width() - w) / 2.0, w)
}

/// The page header's block (round 3b): one title line, centred in it.
pub const HEAD_H: f32 = 40.0;
/// The space between the header and what follows it.
pub const HEAD_GAP: f32 = 20.0;
/// The title keeps at least this much room beside the actions; with less,
/// they go under it.
pub const HEAD_TITLE_MIN: f32 = 320.0;

/// Where [`head`] put things.
pub struct Head {
    /// The actions are on their own row under the title.
    pub stacked: bool,
    /// The actions' 40 px slot: right-align them in it beside the title,
    /// left-align them when stacked.
    pub acts: Rect,
    /// The header's bottom.
    pub bottom: f32,
}

/// The page header (round 3b, one rule for every page): the title alone,
/// on one line in a 40 px block, cut with "…" and the full title on hover.
/// `acts` is the widest the page's actions get (0: none, and a page whose
/// actions change width reserves the widest, so a state change never
/// moves the title): they sit at the right, centred on the title, while
/// that leaves the title [`HEAD_TITLE_MIN`]; else in a row 12 px under it.
pub fn head(ui: &mut Ui, title: &str, x: f32, y: f32, w: f32, acts: f32) -> Head {
    use crate::ui::fonts::Type;
    let ctx = ui.ctx().clone();
    let pal = crate::ui::kit(&ctx).pal;
    let stacked = head_stacked(w, acts);
    let tw = head_room(w, acts);
    let block = text::layout_elided(&ctx, title, Type::H, pal.ink, tw);
    let cut = block.elided();
    let ty = if stacked {
        y
    } else {
        y + (HEAD_H - Type::H.line) / 2.0
    };
    // the page's title: a heading, read out on a new page and when Live's
    // state changes it
    let r = text_as(ui, block, pos2(x, ty), Speak::LiveHeading(1));
    if cut {
        ui.interact(r, ui.id().with("page-title"), egui::Sense::hover())
            .on_hover_text(title);
    }
    if stacked {
        let top = y + Type::H.line + 12.0;
        Head {
            stacked,
            acts: Rect::from_min_size(pos2(x, top), vec2(w, HEAD_H)),
            bottom: top + HEAD_H,
        }
    } else {
        Head {
            stacked,
            acts: Rect::from_min_size(pos2(x + w - acts, y), vec2(acts, HEAD_H)),
            bottom: y + HEAD_H,
        }
    }
}

/// Whether [`head`] puts the actions in a row under the title.
fn head_stacked(w: f32, acts: f32) -> bool {
    acts > 0.0 && w - acts - 24.0 < HEAD_TITLE_MIN
}

/// The title's one line in [`head`] at `w` with `acts` beside it: a title
/// wider than this is cut with "…".
pub fn head_room(w: f32, acts: f32) -> f32 {
    if acts > 0.0 && !head_stacked(w, acts) {
        w - acts - 24.0
    } else {
        w
    }
}

/// The page column's widest content.
pub const COLUMN_MAX: f32 = 800.0;

/// Tell the scroll area (or the parent) how far the page reaches.
pub fn reach(ui: &mut Ui, origin: Pos2, width: f32, bottom: f32) {
    ui.expand_to_include_rect(Rect::from_min_max(origin, pos2(origin.x + width, bottom)));
}

/// "Copy diagnostics" (Live's takeover, Settings › About): the window's
/// [`Copier`](crate::window::diagnostics::Copier), polled once per frame,
/// with the button's words.
pub struct Diagnostics {
    copier: crate::window::diagnostics::Copier,
    view: crate::window::diagnostics::CopyView,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Diagnostics::with(crate::window::diagnostics::report)
    }
}

impl Diagnostics {
    pub fn with(collect: fn() -> String) -> Diagnostics {
        Diagnostics {
            copier: crate::window::diagnostics::Copier::with_collector(collect),
            view: crate::window::diagnostics::CopyView::Idle,
        }
    }

    pub fn start(&mut self) {
        self.copier.start();
    }

    /// Once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.view = self.copier.poll(ctx);
    }

    /// The button's label now.
    pub fn label(&self) -> String {
        use crate::window::diagnostics::CopyView;
        match self.view {
            CopyView::Idle => crate::tr!("copy-diagnostics"),
            CopyView::Collecting => crate::tr!("copy-diagnostics-busy"),
            CopyView::Copied => crate::tr!("copy-diagnostics-done"),
        }
    }
}

/// Open `url` in the desktop's browser: the OpenURI portal (it works in
/// the Flatpak too), else `xdg-open`. Never blocks the frame.
pub fn open_url(url: &str) {
    if cfg!(test) {
        return;
    }
    let url = url.to_owned();
    std::thread::spawn(move || {
        if portal_open(&url).is_err() {
            let _ = std::process::Command::new("xdg-open")
                .arg(&url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    });
}

fn portal_open(url: &str) -> Result<(), zbus::Error> {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.OpenURI",
    )?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let _: zbus::zvariant::OwnedObjectPath = proxy.call("OpenURI", &("", url, options))?;
    Ok(())
}

/// A rect at `x, y` with size `w × h`.
pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_column_is_centred_and_at_most_800_wide() {
        let area = |w: f32, rail: f32| Rect::from_min_max(pos2(rail, 0.0), pos2(w, 600.0));
        // the boards' three sizes: 612 · 712 · 800
        assert_eq!(column(area(720.0, 60.0), true), (84.0, 612.0));
        assert_eq!(column(area(960.0, 184.0), false), (216.0, 712.0));
        assert_eq!(column(area(1440.0, 184.0), false), (412.0, 800.0));
    }
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn a_repeated_name_is_numbered_from_its_second() {
        crate::i18n::pin_english();
        assert_eq!(
            numbered([
                "RODE NT-USB",
                "Built-in Audio",
                "RODE NT-USB",
                "RODE NT-USB"
            ]),
            [
                "RODE NT-USB",
                "Built-in Audio",
                "RODE NT-USB (2)",
                "RODE NT-USB (3)"
            ]
        );
    }

    /// The header (round 3b): one line in 40 px, cut with "…" and named
    /// by its full title; actions beside it while it keeps 320 px.
    #[test]
    fn the_header_is_one_line_with_actions_beside_or_under_it() {
        crate::i18n::pin_english();
        let ctx = egui::Context::default();
        crate::ui::install(&ctx, crate::ui::Theme::Dark);
        let long = "Saved a very long recording of the weekly planning meeting (cleaned).m4a.";
        let mut got = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for (w, acts) in [(300.0, 0.0), (700.0, 300.0), (600.0, 300.0)] {
                    let h = head(ui, long, 0.0, 0.0, w, acts);
                    got.push((h.stacked, h.bottom, h.acts));
                }
            });
        });
        // alone: 40 px
        assert_eq!((got[0].0, got[0].1), (false, HEAD_H));
        // 700 - 300 - 24 = 376 left for the title: beside, on the right
        assert_eq!((got[1].0, got[1].1), (false, HEAD_H));
        assert_eq!(
            got[1].2,
            Rect::from_min_size(pos2(400.0, 0.0), vec2(300.0, HEAD_H))
        );
        // 276 left: under the title, 12 px below its line
        assert!(got[2].0);
        assert_eq!(got[2].2.top(), 28.0 + 12.0);
        assert_eq!(got[2].1, 28.0 + 12.0 + HEAD_H);
        let cut = text::layout_elided(
            &ctx,
            long,
            crate::ui::fonts::Type::H,
            egui::Color32::WHITE,
            300.0,
        );
        assert!(cut.elided());
        assert_eq!(cut.source(), long);
    }

    /// The delay the backend's wake-up hands to the window's loop.
    fn woken(software: bool) -> Duration {
        let ctx = egui::Context::default();
        crate::ui::install(&ctx, crate::ui::Theme::Dark);
        crate::ui::set_software(&ctx, software);
        let got = Arc::new(Mutex::new(Duration::MAX));
        let g = got.clone();
        ctx.set_request_repaint_callback(move |info| {
            let mut g = g.lock().unwrap();
            *g = (*g).min(info.delay);
        });
        wake(&ctx)(Duration::from_millis(5));
        let d = *got.lock().unwrap();
        d
    }

    #[test]
    fn a_software_renderer_wakes_at_the_live_cadence_only() {
        assert!(woken(false) < Duration::from_millis(20));
        // the whole live period (the kit gives back the frame egui takes)
        assert!(
            woken(true) >= Duration::from_millis(99),
            "{:?}",
            woken(true)
        );
    }
}
