//! Settings, one scrolling page in groups: General (autostart,
//! notifications, hidden notices, appearance), Noise removal (the model of
//! the microphone in use), Tray (the icon and its style), Microphones
//! (each one's own strength and model, with Reset), Files (where Save puts
//! cleaned files), Advanced (the model engine, the docs) and About
//! (version, Copy diagnostics, links, credits, licences).

use eframe::egui::{self, pos2, vec2, Align, Rect, Ui};

use super::util::{self, child, rect, text_as, text_at, Speak};
use super::Cx;
use crate::appstate::{Action, AppState, Model, Sound, Strength, TrayHost};
use crate::config::{Appearance, Inference, TrayIcon};
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::icons::Icon;
use crate::ui::paint::mark::{paint_logo, MarkState};
use crate::ui::text;
use crate::ui::tokens::Palette;
use crate::ui::widgets::{self, Button, GroupUi, NoteKind, Segmented, Switch};

pub(super) const SECTION_GAP: f32 = 28.0;

#[derive(Default)]
pub struct SettingsPage {
    to_about: bool,
    to_shortcuts: bool,
    to_files: bool,
    licenses: Licenses,
    place: FilesPlace,
}

/// Settings › Files while a change is under way: the folder chooser is
/// open, or a choice went to the daemon and its state has not caught up
/// (the control shows the choice meanwhile, not the old value for a frame).
#[derive(Default)]
struct FilesPlace {
    pick: Option<std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>>,
    /// The folder asked for (None: next to the original), and when.
    sent: Option<(Option<String>, f64)>,
    /// "In a folder…" is selected and no folder is chosen yet: "Choose a
    /// folder…" opens the chooser (F19: selecting never opens it, so the
    /// arrows can pass over it).
    want: bool,
}

/// How long a choice shows before the daemon's state has the last word
/// (it refused it, or never answered).
const PLACE_SETTLES: f64 = 2.0;

impl FilesPlace {
    /// The folder the section shows: the one asked for while the state
    /// catches up, else the daemon's.
    fn folder(&mut self, state: Option<&String>, now: f64) -> Option<String> {
        if let Some((want, at)) = &self.sent {
            if state == want.as_ref() || now - at > PLACE_SETTLES {
                self.sent = None;
            } else {
                return want.clone();
            }
        }
        state.cloned()
    }

    /// The chooser answered: a folder goes to the daemon.
    fn answered(&mut self, cx: &Cx<'_>) {
        use std::sync::mpsc::TryRecvError;
        let Some(rx) = &self.pick else { return };
        match rx.try_recv() {
            Ok(picked) => {
                self.pick = None;
                if let Some(p) = picked.first() {
                    let dir = super::files::daemon::folder_of(p);
                    self.sent = Some((Some(dir.display().to_string()), cx.now));
                    cx.send(Action::FilesFolder(dir));
                }
            }
            Err(TryRecvError::Disconnected) => self.pick = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    fn choose(&mut self, ctx: &egui::Context) {
        if self.pick.is_some() {
            return;
        }
        let c = ctx.clone();
        let wake: crate::miccheck::Repaint =
            std::sync::Arc::new(move |d| c.request_repaint_after(d));
        self.pick = Some(super::files::desktop::choose(
            tr!("files-choose-folder"),
            super::files::desktop::Pick::Folder,
            wake,
        ));
    }
}

/// About › Licenses: the sheet is open; it closed (the keyboard returns to
/// the link on the next frame: while the sheet is up, nothing under it can
/// take focus).
#[derive(Default)]
struct Licenses {
    open: bool,
    refocus: bool,
}

impl SettingsPage {
    /// Open on About (the tray's and the CLI's "about").
    pub fn scroll_to_about(&mut self) {
        self.to_about = true;
    }

    /// Open on Files (screenshots).
    pub fn scroll_to_files(&mut self) {
        self.to_files = true;
    }

    /// Open on Shortcuts (the CLI's and a notification's "shortcuts").
    pub fn scroll_to_shortcuts(&mut self) {
        self.to_shortcuts = true;
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let s = cx.state;
        if super::status::filtering(s) {
            cx.light.static_edge = true;
        }
        let (pt, _, pb) = if cx.compact {
            (20.0, 24.0, 24.0)
        } else {
            (28.0, 32.0, 40.0)
        };
        let origin = ui.max_rect().min;
        let width = ui.max_rect().width();
        let (x, w) = super::util::column(ui.max_rect(), cx.compact);
        let mut y = origin.y + pt;
        y = super::util::head(ui, &tr!("set-title"), x, y, w, 0.0).bottom + super::util::HEAD_GAP;

        y = section(
            ui,
            x,
            y,
            w,
            &tr!("set-general"),
            None,
            &pal,
            "general",
            |g| general(g, cx, &pal),
        );
        if s.apps.can_set_default {
            y = section(ui, x, y, w, &tr!("set-apps"), None, &pal, "apps", |g| {
                apps(g, cx)
            });
            // the user moved the default away: the setting stays on (the
            // next start takes it back), so the note goes by the release
            if let Some(dev) = s.apps.default_released.as_deref() {
                let device = s.description_of(dev).unwrap_or(dev).to_owned();
                let mut c = child(
                    ui,
                    rect(x, y - SECTION_GAP + 10.0, w, 200.0),
                    "set-released",
                );
                let act = tr!("set-default-again");
                let link = widgets::note(
                    &mut c,
                    NoteKind::Info,
                    &tr!("set-default-released-title"),
                    &tr!("set-default-released-body", device = device),
                    Some(&act),
                );
                if link.is_some_and(|l| l.clicked()) {
                    cx.send(Action::SetDefaultMic(true));
                }
                y = c.min_rect().bottom() + SECTION_GAP;
            }
        }
        let shortcuts_top = y;
        y = super::shortcuts::section(ui, cx, x, y, w) + SECTION_GAP;
        if std::mem::take(&mut self.to_shortcuts) {
            ui.scroll_to_rect(
                Rect::from_min_size(pos2(x, shortcuts_top - 24.0), vec2(w, 10.0)),
                Some(Align::Min),
            );
        }
        y = section(ui, x, y, w, &tr!("set-sound"), None, &pal, "sound", |g| {
            sound(g, cx)
        });
        // F5: where Save puts files, in view without scrolling past every
        // microphone
        let place = &mut self.place;
        place.answered(cx);
        let files_top = y;
        y = section(ui, x, y, w, &tr!("set-files"), None, &pal, "files", |g| {
            files(g, cx, place)
        });
        if std::mem::take(&mut self.to_files) {
            ui.scroll_to_rect(
                Rect::from_min_size(pos2(x, files_top - 24.0), vec2(w, 10.0)),
                Some(Align::Min),
            );
        }
        let no_tray = s.tray == TrayHost::Waiting;
        y = section(ui, x, y, w, &tr!("set-tray"), None, &pal, "tray", |g| {
            tray(g, cx)
        });
        if no_tray {
            let mut c = child(ui, rect(x, y - SECTION_GAP + 10.0, w, 200.0), "set-no-tray");
            widgets::note(
                &mut c,
                NoteKind::Info,
                &tr!("set-no-tray-title"),
                &tr!("set-no-tray-body"),
                None,
            );
            y = c.min_rect().bottom() + SECTION_GAP;
        }
        let mics_cap = tr!("set-mics-cap");
        y = section(
            ui,
            x,
            y,
            w,
            &tr!("set-mics"),
            Some(&mics_cap),
            &pal,
            "mics",
            |g| mics(g, cx),
        );
        y = section(
            ui,
            x,
            y,
            w,
            &tr!("set-advanced"),
            None,
            &pal,
            "advanced",
            |g| advanced(g, cx),
        );
        let about_top = y;
        let licenses = &mut self.licenses;
        y = section(ui, x, y, w, &tr!("set-about"), None, &pal, "about", |g| {
            about(g, cx, &pal, licenses)
        });
        y -= SECTION_GAP;
        if std::mem::take(&mut self.to_about) {
            ui.scroll_to_rect(
                Rect::from_min_size(pos2(x, about_top - 24.0), vec2(w, 10.0)),
                Some(Align::Min),
            );
        }
        util::reach(ui, origin, width, y + pb);
        if self.licenses.open {
            licenses_sheet(&ctx, &mut self.licenses.open);
            if !self.licenses.open {
                self.licenses.refocus = true;
                ctx.request_repaint();
            }
        }
    }
}

/// An eyebrow heading, an optional caption, and a group of rows. Returns
/// where the next section starts.
#[allow(clippy::too_many_arguments)]
fn section(
    ui: &mut Ui,
    x: f32,
    y: f32,
    w: f32,
    title: &str,
    caption: Option<&str>,
    pal: &Palette,
    salt: &str,
    rows: impl FnOnce(&mut GroupUi<'_>),
) -> f32 {
    let ctx = ui.ctx().clone();
    let mut y = text_as(
        ui,
        text::layout(&ctx, title, Type::EYEBROW, pal.ink_subtle, w),
        pos2(x, y),
        Speak::Heading(2),
    )
    .bottom()
        + 10.0;
    if let Some(c) = caption {
        // `margin-top: -4px`
        y = text_at(
            ui,
            text::layout(&ctx, c, Type::CAP, pal.ink_subtle, w),
            pos2(x, y - 4.0),
        )
        .bottom()
            + 10.0;
    }
    let mut c = child(ui, rect(x, y, w, 4000.0), ("set-group", salt));
    widgets::group(&mut c, rows);
    c.min_rect().bottom() + SECTION_GAP
}

/// A settings row: label (and caption) on the left, the control on the
/// right; the control wraps under the text when the row is too narrow.
/// `named`: the control carries the label as its name (switches, choices),
/// so the label is painted without a node of its own.
#[allow(clippy::too_many_arguments)]
pub(super) fn srow(
    g: &mut GroupUi<'_>,
    label: &str,
    caption: &str,
    named: bool,
    control_w: f32,
    control_h: f32,
    control: impl FnOnce(&mut Ui),
) {
    srow_keyed(
        g, label, label, caption, named, control_w, control_h, control,
    );
}

/// [`srow`] whose control is known by `key`, where two rows may carry the
/// same label (two microphones with one name).
#[allow(clippy::too_many_arguments)]
fn srow_keyed(
    g: &mut GroupUi<'_>,
    key: impl std::hash::Hash,
    label: &str,
    caption: &str,
    named: bool,
    control_w: f32,
    control_h: f32,
    control: impl FnOnce(&mut Ui),
) {
    g.row(|ui| {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let w = ui.available_width();
        let side = w - control_w - 12.0 >= 260.0 || control_w == 0.0;
        let tw = if side && control_w > 0.0 {
            w - control_w - 12.0
        } else {
            w
        };
        let lb = text::layout(&ctx, label, Type::LABEL, pal.ink, tw);
        let cb = (!caption.is_empty())
            .then(|| text::layout(&ctx, caption, Type::CAP, pal.ink_subtle, tw));
        let text_h = lb.size().y + cb.as_ref().map_or(0.0, |c| 2.0 + c.size().y);
        let o = ui.cursor().min;
        let row_h = if side {
            text_h.max(control_h)
        } else {
            text_h + 12.0 + control_h
        };
        let ty = if side {
            o.y + (row_h - text_h) / 2.0
        } else {
            o.y
        };
        let r = if named {
            util::paint_at(ui, lb, pos2(o.x, ty))
        } else {
            text_at(ui, lb, pos2(o.x, ty))
        };
        // a control named by the row's label comes before the caption, so
        // a screen reader reads the caption after the control it explains
        let mut control = Some(control);
        let mut place_control = |ui: &mut Ui| {
            if let (Some(f), true) = (control.take(), control_w > 0.0) {
                let (cxp, cyp) = if side {
                    (o.x + w - control_w, o.y + (row_h - control_h) / 2.0)
                } else {
                    (o.x, o.y + text_h + 12.0)
                };
                let mut c = child(ui, rect(cxp, cyp, control_w, control_h), ("srow", &key));
                f(&mut c);
            }
        };
        if named {
            place_control(ui);
        }
        if let Some(cb) = cb {
            text_at(ui, cb, pos2(o.x, r.bottom() + 2.0));
        }
        place_control(ui);
        ui.advance_cursor_after_rect(Rect::from_min_size(o, vec2(w, row_h)));
    });
}

fn switch_row(g: &mut GroupUi<'_>, label: &str, caption: &str, on: bool, send: impl FnOnce(bool)) {
    let mut v = on;
    let mut changed = false;
    // the row keeps the switch's 29 px box; the 24 px track is centred in it
    srow(g, label, caption, true, 44.0, 29.0, |ui| {
        ui.add_space((29.0 - 24.0) / 2.0);
        changed = ui.add(Switch::new(&mut v).name(label)).changed();
    });
    if changed {
        send(v);
    }
}

/// Settings › Apps: the default microphone (round 3b: it was on Live).
fn apps(g: &mut GroupUi<'_>, cx: &mut Cx<'_>) {
    let s = cx.state;
    switch_row(
        g,
        &tr!("set-default-mic"),
        &tr!("set-default-mic-cap"),
        s.apps.set_default,
        |v| cx.send(Action::SetDefaultMic(v)),
    );
}

fn general(g: &mut GroupUi<'_>, cx: &mut Cx<'_>, _pal: &Palette) {
    let s = cx.state;
    switch_row(g, &tr!("set-autostart"), "", s.prefs.autostart, |v| {
        cx.send(Action::SetAutostart(v))
    });
    switch_row(
        g,
        &tr!("set-notifications"),
        &tr!("set-notifications-cap"),
        s.prefs.notifications,
        |v| cx.send(Action::SetNotifications(v)),
    );
    let (sys, dark, light) = (
        tr!("set-appearance-system"),
        tr!("set-appearance-dark"),
        tr!("set-appearance-light"),
    );
    let opts = [
        (Appearance::System, sys.as_str()),
        (Appearance::Dark, dark.as_str()),
        (Appearance::Light, light.as_str()),
    ];
    let mut v = s.prefs.appearance;
    let mut changed = false;
    let name = tr!("set-appearance");
    srow(g, &name, "", true, 240.0, 38.0, |ui| {
        changed = Segmented::new(&name, &mut v, &opts)
            .width(240.0)
            .id_salt("set-appearance")
            .show(ui)
            .changed();
    });
    if changed {
        cx.send(Action::SetAppearance(v));
    }
}

/// The model, for the microphone in use (each microphone keeps its own).
fn sound(g: &mut GroupUi<'_>, cx: &mut Cx<'_>) {
    let (q, l) = (tr!("model-quality"), tr!("model-light"));
    let opts = [(Model::Quality, q.as_str()), (Model::Light, l.as_str())];
    let mut v = cx.state.sound.current.model;
    let mut changed = false;
    let name = tr!("set-model");
    srow(g, &name, &tr!("set-model-cap"), true, 200.0, 38.0, |ui| {
        changed = Segmented::new(&name, &mut v, &opts)
            .width(200.0)
            .id_salt("set-model")
            .show(ui)
            .changed();
    });
    if changed {
        cx.send(Action::SetModel(v));
    }
    switch_row(
        g,
        &tr!("set-voice-hold"),
        &tr!("set-voice-hold-cap"),
        cx.state.prefs.voice_hold,
        |v| cx.send(Action::SetVoiceHold(v)),
    );
}

fn tray(g: &mut GroupUi<'_>, cx: &mut Cx<'_>) {
    let s = cx.state;
    switch_row(
        g,
        &tr!("set-tray-show"),
        &tr!("set-tray-show-cap"),
        s.prefs.tray,
        |v| cx.send(Action::SetTray(v)),
    );
    let (a, c, m) = (
        tr!("set-tray-icon-auto"),
        tr!("set-tray-icon-color"),
        tr!("set-tray-icon-mono"),
    );
    let opts = [
        (TrayIcon::Auto, a.as_str()),
        (TrayIcon::Color, c.as_str()),
        (TrayIcon::Symbolic, m.as_str()),
    ];
    let mut v = s.prefs.tray_icon;
    let mut changed = false;
    let name = tr!("set-tray-icon");
    let on = s.prefs.tray;
    srow(
        g,
        &name,
        &tr!("set-tray-icon-cap"),
        true,
        300.0,
        38.0,
        |ui| {
            // dims (and stops taking clicks) while the tray icon is off
            let k = widgets::anim(ui.ctx(), egui::Id::new("set-tray-icon-dim"), !on, 0.16);
            ui.set_opacity(1.0 - 0.6 * k);
            ui.add_enabled_ui(on, |ui| {
                changed = Segmented::new(&name, &mut v, &opts)
                    .width(300.0)
                    .id_salt("set-tray-icon")
                    .show(ui)
                    .changed();
            });
        },
    );
    if changed && on {
        cx.send(Action::SetTrayIcon(v));
    }
}

/// "Maximum · Quality".
pub fn sound_words(sound: &Sound) -> String {
    let strength = match sound.strength {
        Strength::Gentle => tr!("strength-gentle"),
        Strength::Medium => tr!("strength-medium"),
        Strength::Strong => tr!("strength-strong"),
        Strength::Maximum => tr!("strength-maximum"),
        Strength::Custom => tr!("set-mic-custom"),
    };
    let model = match sound.model {
        Model::Quality => tr!("model-quality"),
        Model::Light => tr!("model-light"),
    };
    tr!("set-mic-sound", strength = strength, model = model)
}

fn mics(g: &mut GroupUi<'_>, cx: &mut Cx<'_>) {
    let s: &AppState = cx.state;
    let reset = tr!("set-mic-reset");
    // the microphone in use, by name, also before it has settings of its
    // own (UX panel B2): it runs on the defaults until its first change
    let in_use = |device: &str| s.sound.owner.as_deref() == Some(device);
    if let Some(owner) = s.sound.owner.as_deref().filter(|_| !s.sound.saved) {
        let name = s.description_of(owner).unwrap_or(owner).to_owned();
        srow_keyed(
            g,
            owner,
            &name,
            &tr!("set-mic-in-use", sound = sound_words(&s.sound.current)),
            false,
            0.0,
            0.0,
            |_| {},
        );
    }
    // the snapshot tells same-name microphones apart ("RODE NT-USB (2)");
    // the rows go by device all the same
    for p in &s.sound.profiles {
        let btn = Button::secondary(&reset).sm();
        let bw = btn.width(&g.ui().ctx().clone());
        let mut clicked = false;
        let name = if p.description.is_empty() {
            p.device.clone()
        } else {
            p.description.clone()
        };
        // "Reset RODE NT-USB" for screen readers: the rows' buttons all
        // say "Reset"
        let a11y = tr!("set-mic-reset-name", mic = name.clone());
        let words = if in_use(&p.device) {
            tr!("set-mic-in-use", sound = sound_words(&p.sound))
        } else {
            sound_words(&p.sound)
        };
        srow_keyed(g, &p.device, &name, &words, false, bw, 32.0, |ui| {
            let r = ui.add(btn);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &a11y));
            clicked = r.clicked();
        });
        if clicked {
            cx.send(Action::ResetProfile(p.device.clone()));
        }
    }
    srow(
        g,
        &tr!("set-mics-others"),
        &sound_words(&s.sound.defaults),
        false,
        0.0,
        0.0,
        |_| {},
    );
}

/// A segmented control's width when every option shows in full: the
/// widest label, the option's 12 px padding and the thumb's slack, times
/// the options, plus the frame.
fn segmented_width(ctx: &egui::Context, labels: &[&str]) -> f32 {
    let widest = labels
        .iter()
        .map(|l| {
            text::layout(
                ctx,
                l,
                Type::OPTION.with_line(17.0),
                egui::Color32::WHITE,
                f32::INFINITY,
            )
            .size()
            .x
        })
        .fold(0.0, f32::max);
    labels.len() as f32 * (widest + 24.0) + 8.0
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Place {
    Next,
    Folder,
}

/// Files: "Save cleaned files: Next to the original · In a folder…"
/// (App-Files, Around: Settings › Files), the label, the choice and its
/// caption stacked. In the Flatpak, which cannot write next to the
/// original, the folder and Change… instead. Then where "Open with" is.
fn files(g: &mut GroupUi<'_>, cx: &mut Cx<'_>, place: &mut FilesPlace) {
    let s = cx.state;
    let folder = place.folder(s.files.folder.as_ref(), cx.now);
    let sandbox = s.files.needs_folder;
    let name = tr!("set-files-save");
    let mut choose = false;
    let mut chosen = None;
    g.row(|ui| {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let w = ui.available_width();
        let o = ui.cursor().min;
        let (n, f) = (tr!("set-files-next"), tr!("set-files-folder"));
        // the choice sits at the row's right like every other control
        // (UX panel A24) when it fits beside the label, else under it
        let need = segmented_width(&ctx, &[n.as_str(), f.as_str()]).max(260.0);
        let side = !sandbox && w - need - 12.0 >= 260.0;
        let sw = if side { need } else { w.min(need.max(360.0)) };
        let tw = if side { w - sw - 12.0 } else { w };
        let lb = text::layout(&ctx, &name, Type::LABEL, pal.ink, tw);
        let ly = if side {
            o.y + ((38.0 - lb.size().y) / 2.0).max(0.0)
        } else {
            o.y
        };
        // the choice carries the label as its name
        let lr = if sandbox {
            text_at(ui, lb, pos2(o.x, ly))
        } else {
            util::paint_at(ui, lb, pos2(o.x, ly))
        };
        let mut y = lr.bottom() + 8.0;
        if !sandbox {
            let opts = [(Place::Next, n.as_str()), (Place::Folder, f.as_str())];
            let mut v = if folder.is_some() || place.pick.is_some() || place.want {
                Place::Folder
            } else {
                Place::Next
            };
            let (sx, sy) = if side { (o.x + w - sw, o.y) } else { (o.x, y) };
            let mut c = child(ui, rect(sx, sy, sw, 38.0), "set-files-place");
            if Segmented::new(&name, &mut v, &opts)
                .width(sw)
                .id_salt("set-files-place")
                .show(&mut c)
                .changed()
            {
                chosen = Some(v);
            }
            y = if side {
                lr.bottom() + 2.0
            } else {
                y + 38.0 + 8.0
            };
        }
        // the caption, and after a folder its Change… (Choose… while none)
        let line = match (&folder, sandbox) {
            (Some(f), _) => Some(super::files::daemon::folder_shown(std::path::Path::new(f))),
            (None, true) => Some(tr!("set-files-unset")),
            // selected, none chosen yet: the link alone
            (None, false) if place.want => Some(String::new()),
            (None, false) => None,
        };
        let bottom = match line {
            Some(words) if place.pick.is_none() || sandbox => {
                let link = match (&folder, sandbox) {
                    (Some(_), _) => tr!("set-files-change"),
                    (None, true) => tr!("set-files-choose"),
                    (None, false) => tr!("set-files-choose-folder"),
                };
                let lw = text::layout(&ctx, &link, Type::LINK, pal.ink, tw).size().x;
                let r = if words.is_empty() {
                    Rect::from_min_size(pos2(o.x, y + 1.0), egui::Vec2::ZERO)
                } else {
                    let words = format!("{words} · ");
                    let cb =
                        text::layout(&ctx, &words, Type::CAP, pal.ink_subtle, (tw - lw).max(80.0));
                    text_at(ui, cb, pos2(o.x, y + 1.0))
                };
                let lh = text::layout(&ctx, &link, Type::LINK, pal.ink, tw).size().y;
                let (lx, ly) = if r.right() + lw <= o.x + tw {
                    (r.right(), y)
                } else {
                    (o.x, r.bottom() + 4.0)
                };
                let mut c = child(ui, rect(lx, ly, lw + 4.0, lh), "set-files-change");
                let resp = widgets::link(&mut c, &link, false, false);
                let label = if folder.is_some() {
                    tr!("set-files-change-name")
                } else {
                    tr!("set-files-choose-name")
                };
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Link, true, &label)
                });
                choose = resp.clicked();
                r.bottom().max(ly + lh)
            }
            _ => {
                let cap = if place.pick.is_some() {
                    tr!("set-files-choosing")
                } else {
                    tr!("set-files-next-cap")
                };
                let cb = text::layout(&ctx, &cap, Type::CAP, pal.ink_subtle, tw);
                text_at(ui, cb, pos2(o.x, y)).bottom()
            }
        };
        let bottom = if sandbox {
            let cb = text::layout(
                &ctx,
                &tr!("set-files-sandbox-cap"),
                Type::CAP,
                pal.ink_subtle,
                tw,
            );
            text_at(ui, cb, pos2(o.x, bottom + 8.0)).bottom()
        } else {
            bottom
        };
        // a short caption beside the choice still ends under it
        let bottom = if side { bottom.max(o.y + 38.0) } else { bottom };
        // the file manager's "Clean with HushMic": help text, not a row
        // that looks like a setting (UX panel, newcomer); the AppImage
        // installs no "Open with" entry
        let bottom = if std::env::var_os("APPIMAGE").is_none() {
            let cb = text::layout(
                &ctx,
                &tr!("set-files-open-with-help"),
                Type::CAP,
                pal.ink_subtle,
                w,
            );
            text_at(ui, cb, pos2(o.x, bottom + 12.0)).bottom()
        } else {
            bottom
        };
        ui.advance_cursor_after_rect(Rect::from_min_max(o, pos2(o.x + w, bottom)));
    });
    let ctx = g.ui().ctx().clone();
    match chosen {
        // selected only: the chooser opens from "Choose a folder…" (F19)
        Some(Place::Folder) => place.want = true,
        Some(Place::Next) => {
            place.want = false;
            place.pick = None;
            place.sent = Some((None, cx.now));
            cx.send(Action::FilesNextToOriginal);
        }
        None => {}
    }
    if choose {
        place.choose(&ctx);
    }
}

fn advanced(g: &mut GroupUi<'_>, cx: &mut Cx<'_>) {
    let s = cx.state;
    let (a, o) = (tr!("set-engine-auto"), tr!("set-engine-onnx"));
    let opts = [(Inference::Auto, a.as_str()), (Inference::Onnx, o.as_str())];
    let mut v = s.prefs.inference;
    let mut changed = false;
    let name = tr!("set-engine");
    srow(g, &name, &tr!("set-engine-cap"), true, 260.0, 38.0, |ui| {
        changed = Segmented::new(&name, &mut v, &opts)
            .width(260.0)
            .id_salt("set-engine")
            .show(ui)
            .changed();
    });
    if changed {
        cx.send(Action::SetInference(v));
    }
    g.row(|ui| {
        // the links' own line, not the kit's 32 px interaction height
        ui.spacing_mut().interact_size.y = 0.0;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(20.0, 8.0);
            if util::row_link(ui, &tr!("set-terminal"), true, false).clicked() {
                util::open_url(util::url::USAGE);
            }
            if util::row_link(ui, &tr!("set-headless"), true, false).clicked() {
                util::open_url(util::url::HEADLESS);
            }
        });
    });
}

fn about(g: &mut GroupUi<'_>, cx: &mut Cx<'_>, pal: &Palette, licenses: &mut Licenses) {
    let s = cx.state;
    g.row_with(egui::Margin::symmetric(16, 20), |ui| {
        let ctx = ui.ctx().clone();
        let o = ui.cursor().min;
        paint_logo(ui.painter(), o, 48.0, MarkState::Active, pal);
        let tw = ui.available_width() - 64.0;
        let nb = text::layout(&ctx, "HushMic", Type::T3, pal.ink, tw);
        let version = s.daemon.version.clone();
        let vb = text::layout(
            &ctx,
            &tr!("set-version", version = version),
            Type::CAP,
            pal.ink_subtle,
            tw,
        );
        let th = nb.size().y + 2.0 + vb.size().y;
        let ty = o.y + ((48.0 - th) / 2.0).max(0.0);
        let r = text_at(ui, nb, pos2(o.x + 64.0, ty));
        let r2 = text_at(ui, vb, pos2(o.x + 64.0, r.bottom() + 2.0));
        ui.advance_cursor_after_rect(Rect::from_min_max(
            o,
            pos2(o.x + 64.0 + tw, r2.bottom().max(o.y + 48.0)),
        ));
    });
    g.row(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(20.0, 10.0);
            let label = cx.diag.label();
            if ui
                .add(Button::secondary(&label).sm().icon(Icon::Copy))
                .clicked()
            {
                cx.diag.start();
            }
            if util::row_link(ui, &tr!("set-website"), true, false).clicked() {
                util::open_url(util::url::WEBSITE);
            }
            if util::row_link(ui, &tr!("set-report"), true, false).clicked() {
                util::open_url(util::url::ISSUES);
            }
            let r = util::row_link(ui, &tr!("set-licenses"), false, false);
            if std::mem::take(&mut licenses.refocus) {
                r.request_focus();
            }
            if r.clicked() {
                licenses.open = true;
            }
        });
    });
    g.row(|ui| {
        // a caption inline in a plain 16 px block: its lines take the
        // block's 21 px line boxes (Geist's normal line height at 16 px)
        // and sit on the block's baseline, 16 px below each line's top
        let ty = Type::CAP.with_line(21.0);
        let drop = (1.005_f32 * 16.0).round() - ty.baseline_at(1.0);
        // the name is the sentence, not the wrapped lines
        let words = tr!("set-credits");
        let block = text::layout(ui.ctx(), &words, ty, pal.ink_subtle, ui.available_width());
        let (r, resp) = ui.allocate_exact_size(block.size(), egui::Sense::hover());
        block.paint(ui.painter(), r.min + vec2(0.0, drop));
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &words));
    });
}

/// About › Licenses at one width and theme: where each entry sits. The
/// texts are long (hundreds of crates): the monospaced ones are wrapped by
/// their character count and laid out only when they come into view.
#[derive(Clone, Default)]
struct LaidLicenses {
    key: (u32, egui::Color32),
    items: Vec<LicenseItem>,
    height: f32,
}

#[derive(Clone)]
struct LicenseItem {
    y: f32,
    /// The text, with its lines already broken for a monospaced one.
    words: String,
    ty: Type,
    color: egui::Color32,
    block: Option<text::TextBlock>,
}

/// Lines of a monospaced licence text laid out together.
const MONO_PIECE: usize = 200;

/// `text` broken into lines of at most `cols` characters, at spaces where
/// it can (a longer word is cut).
fn mono_wrap(text: &str, cols: usize) -> String {
    let cols = cols.max(8);
    let mut out = String::with_capacity(text.len() + text.len() / cols);
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut rest = line;
        while rest.chars().count() > cols {
            let cut = rest.char_indices().nth(cols).map_or(rest.len(), |(i, _)| i);
            let at = rest[..cut].rfind(' ').filter(|&i| i > 0).unwrap_or(cut);
            out.push_str(rest[..at].trim_end());
            out.push('\n');
            rest = rest[at..].trim_start_matches(' ');
        }
        out.push_str(rest);
    }
    out
}

/// The entries placed at `width` (cached); the blocks are laid out by
/// [`license_block`].
fn laid_licenses(ctx: &egui::Context, width: f32) -> (egui::Id, f32) {
    let pal = crate::ui::kit(ctx).pal;
    let key = ((width * 4.0) as u32, pal.ink);
    let id = egui::Id::new("hm-licenses-laid");
    let cached = ctx.data_mut(|d| {
        let laid = d.get_temp_mut_or_default::<LaidLicenses>(id);
        (laid.key == key && !laid.items.is_empty()).then_some(laid.height)
    });
    if let Some(height) = cached {
        return (id, height);
    }
    // laid out outside the data lock (layout takes the context's)
    let advance = text::width(ctx, "M", Type::MONO).max(1.0);
    let cols = ((width + 1.0 / 64.0) / advance) as usize;
    let mut y = 0.0;
    let mut items = Vec::new();
    for (words, ty, muted, after) in license_entries() {
        let color = if muted { pal.ink_muted } else { pal.ink };
        if ty == Type::MONO {
            // long texts (ONNX Runtime's notices) in pieces, each laid out
            // when it comes into view
            let wrapped = mono_wrap(&words, cols);
            let lines: Vec<&str> = wrapped.split('\n').collect();
            let pieces = lines.chunks(MONO_PIECE).count();
            for (i, piece) in lines.chunks(MONO_PIECE).enumerate() {
                items.push(LicenseItem {
                    y,
                    words: piece.join("\n"),
                    ty,
                    color,
                    block: None,
                });
                y += piece.len() as f32 * ty.line;
                if i + 1 == pieces {
                    y += after;
                }
            }
            continue;
        }
        let block = text::layout(ctx, &words, ty, color, width);
        let h = block.size().y;
        items.push(LicenseItem {
            y,
            words,
            ty,
            color,
            block: Some(block),
        });
        y += h + after;
    }
    let height = y;
    ctx.data_mut(|d| d.insert_temp(id, LaidLicenses { key, items, height }));
    (id, height)
}

/// The entries between `top` and `bottom` (content coordinates): their
/// tops and blocks, laid out now if they weren't.
fn license_blocks(
    ctx: &egui::Context,
    id: egui::Id,
    top: f32,
    bottom: f32,
) -> Vec<(f32, text::TextBlock)> {
    let todo: Vec<(usize, String, Type, egui::Color32)> = ctx.data_mut(|d| {
        let laid = d.get_temp_mut_or_default::<LaidLicenses>(id);
        let next = |i: usize| laid.items.get(i + 1).map_or(laid.height, |n| n.y);
        (0..laid.items.len())
            .filter(|&i| {
                laid.items[i].block.is_none() && next(i) >= top && laid.items[i].y <= bottom
            })
            .map(|i| {
                let it = &laid.items[i];
                (i, it.words.clone(), it.ty, it.color)
            })
            .collect()
    });
    // laid out outside the data lock (layout takes the fonts')
    let made: Vec<(usize, text::TextBlock)> = todo
        .into_iter()
        .map(|(i, w, ty, c)| (i, text::layout(ctx, &w, ty, c, f32::INFINITY)))
        .collect();
    ctx.data_mut(|d| {
        let laid = d.get_temp_mut_or_default::<LaidLicenses>(id);
        for (i, b) in made {
            laid.items[i].block = Some(b);
        }
        let height = laid.height;
        let items = &laid.items;
        (0..items.len())
            .filter(|&i| {
                let next = items.get(i + 1).map_or(height, |n| n.y);
                next >= top && items[i].y <= bottom
            })
            .filter_map(|i| Some((items[i].y, items[i].block.clone()?)))
            .collect()
    })
}

/// What About › Licenses lists: the fonts and icons, then the Rust crates
/// (names, licence, where the source is for MPL, the texts), as (text,
/// style, muted, space after).
fn license_entries() -> Vec<(String, Type, bool, f32)> {
    let mut out = Vec::new();
    for l in crate::ui::licenses::LICENSES {
        out.push((l.name.to_owned(), Type::LABEL, false, 6.0));
        out.push((l.text.to_owned(), Type::MONO, true, 16.0));
    }
    out.push((tr!("set-licenses-crates"), Type::EYEBROW, true, 12.0));
    for g in crate::ui::licenses::GROUPS {
        out.push((g.names(), Type::LABEL, false, 4.0));
        out.push((g.license.to_owned(), Type::CAP, true, 6.0));
        for url in g.sources() {
            out.push((tr!("set-licenses-source", url = url), Type::CAP, true, 6.0));
        }
        for t in g.texts {
            out.push(((*t).to_owned(), Type::MONO, true, 10.0));
        }
        if let Some(last) = out.last_mut() {
            last.3 = 16.0;
        }
    }
    out
}

/// About › Licenses: the licences of what the window embeds.
fn licenses_sheet(ctx: &egui::Context, open: &mut bool) {
    let pal = crate::ui::kit(ctx).pal;
    let screen = ctx.content_rect();
    let w = (screen.width() - 64.0).min(620.0);
    let h = (screen.height() - 64.0).min(520.0);
    let r = Rect::from_center_size(screen.center(), vec2(w, h));
    let modal = egui::Modal::new(egui::Id::new("hm-licenses"))
        .backdrop_color(pal.scrim)
        .frame(
            egui::Frame::new()
                .fill(pal.surface_raised)
                .stroke(egui::Stroke::new(1.0_f32, pal.line))
                .corner_radius(12)
                .inner_margin(20),
        )
        .show(ctx, |outer| {
            widgets::set_backdrop(outer, pal.surface_raised);
            outer.set_width(r.width() - 40.0);
            outer.set_height(r.height() - 40.0);
            let title = tr!("set-licenses-title");
            // one dialog for screen readers, named Licenses; the page
            // behind out of reach while it is up
            let mut d = widgets::a11y_children(
                outer,
                outer.max_rect(),
                egui::accesskit::Role::Dialog,
                &title,
            );
            widgets::a11y(ctx, d.unique_id(), |n| n.set_modal());
            let ui = &mut d;
            text::label(ui, &title, Type::T3, pal.ink);
            ui.add_space(12.0);
            // no drag-to-scroll: the text area would be an unnamed Tab stop
            let out = egui::ScrollArea::vertical()
                .scroll_source(egui::scroll_area::ScrollSource {
                    drag: false,
                    ..egui::scroll_area::ScrollSource::ALL
                })
                .max_height(r.height() - 40.0 - 24.0 - 12.0 - 52.0)
                .show_viewport(ui, |ui, view| {
                    let width = ui.available_width();
                    let (id, height) = laid_licenses(ui.ctx(), width);
                    let origin = ui.min_rect().min;
                    ui.set_min_size(vec2(width, height));
                    for (y, block) in license_blocks(ui.ctx(), id, view.top(), view.bottom()) {
                        let r = Rect::from_min_size(origin + vec2(0.0, y), block.size());
                        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r));
                        text::show(&mut c, block);
                    }
                });
            // the texts are no controls: the bar is how the keyboard reads on
            widgets::scroll::bar(ui, &out, &title);
            ui.add_space(12.0);
            let close = tr!("set-licenses-close");
            let clicked = ui.add(Button::secondary(&close).sm()).clicked();
            let used = d.min_rect();
            outer.advance_cursor_after_rect(used);
            clicked
        });
    if modal.inner || modal.should_close() {
        *open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{state, Scenario};
    use crate::appstate::Page;
    use crate::pages::testing;
    use egui_kittest::kittest::{NodeT, Queryable};

    /// The option `label` of the choice named `choice` ("Light" is both an
    /// appearance and a model).
    fn option<'a>(
        w: &'a testing::Window,
        choice: &'a str,
        label: &'a str,
    ) -> egui_kittest::Node<'a> {
        testing::by_role(&w.harness, choice, testing::Role::RadioGroup).get_by_label(label)
    }

    #[test]
    fn every_switch_and_choice_sends_its_action() {
        let mut w = testing::window(
            state(Scenario::OnInCall),
            Page::Settings,
            egui::vec2(960.0, 2000.0),
        );
        w.harness
            .get_by_label("Start HushMic when you log in")
            .click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Notifications").click();
        w.harness.run_steps(2);
        option(&w, "Appearance", "Light").click();
        w.harness.run_steps(2);
        option(&w, "Model", "Light").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Stay on my voice").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Show tray icon").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Monochrome").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("ONNX Runtime").click();
        w.harness.run_steps(2);
        assert_eq!(
            w.link.sent(),
            vec![
                Action::SetAutostart(false),
                Action::SetNotifications(false),
                Action::SetAppearance(Appearance::Light),
                Action::SetModel(Model::Light),
                Action::SetVoiceHold(true),
                Action::SetTray(false),
                Action::SetTrayIcon(TrayIcon::Symbolic),
                Action::SetInference(Inference::Onnx),
            ]
        );
    }

    fn files_state(folder: Option<&str>, sandbox: bool) -> AppState {
        let mut s = state(Scenario::OnInCall);
        s.files.folder = folder.map(str::to_owned);
        s.files.needs_folder = sandbox;
        s
    }

    fn files_window(s: AppState) -> testing::Window {
        let mut w = testing::window(s, Page::Settings, egui::vec2(960.0, 2400.0));
        w.harness.run_steps(2);
        w
    }

    #[test]
    fn the_files_choice_sits_at_the_right_like_every_control() {
        // wide: on the label's line, its right edge where Model engine's is
        let w = files_window(files_state(None, false));
        let folder = option(&w, "Save cleaned files", "In a folder…").rect();
        let onnx = option(&w, "Model engine", "ONNX Runtime").rect();
        let cap = "Saved again at another strength, the copy’s name says it: “(cleaned, Gentle)”.";
        let caption = w.harness.get_by_label(cap).rect();
        assert!(
            (folder.right() - onnx.right()).abs() < 0.5,
            "{folder:?} {onnx:?}"
        );
        // the caption under the label, beside the choice
        assert!(caption.right() < folder.left(), "{caption:?} {folder:?}");
        assert!(caption.top() < folder.bottom(), "{caption:?} {folder:?}");
        // too narrow for both: under the label, from its left edge
        let mut w = testing::window(
            files_state(None, false),
            Page::Settings,
            egui::vec2(560.0, 2400.0),
        );
        w.harness.run_steps(2);
        let next = option(&w, "Save cleaned files", "Next to the original").rect();
        let caption = w.harness.get_by_label(cap).rect();
        assert!(next.bottom() < caption.top(), "{next:?} {caption:?}");
        assert!(next.left() - caption.left() < 8.0, "{next:?} {caption:?}");
    }

    #[test]
    fn files_save_next_to_the_original_or_in_a_chosen_folder() {
        use crate::pages::files::desktop::answer_next;
        let dir = std::env::temp_dir();
        // Next to the original; "In a folder…" asks for the folder.
        let mut w = files_window(files_state(None, false));
        w.harness.get_by_label(
            "Saved again at another strength, the copy’s name says it: “(cleaned, Gentle)”.",
        );
        // the file manager's entry is help text under it, not a row
        {
            use egui_kittest::kittest::NodeT;
            assert!(w
                .harness
                .query_by_label("Open files with HushMic")
                .is_none());
            let help = w.harness.get_by_label_contains("“Clean with HushMic”");
            assert_eq!(help.accesskit_node().role(), egui::accesskit::Role::Label);
        }
        // F19: selecting "In a folder…" (the arrows, a click) opens
        // nothing; "Choose a folder…" does
        answer_next(vec![dir.clone()]);
        option(&w, "Save cleaned files", "In a folder…").click();
        w.harness.run_steps(3);
        assert!(w.link.sent().is_empty(), "no chooser yet");
        assert!(w
            .harness
            .query_by_label("Choose a folder for cleaned files")
            .is_some());
        w.harness
            .get_by_label("Choose a folder for cleaned files")
            .click();
        w.harness.run_steps(3);
        assert_eq!(w.link.sent(), vec![Action::FilesFolder(dir.clone())]);
        // until the state has it, the choice shows the folder
        let words = crate::pages::files::daemon::folder_shown(&dir);
        w.harness.get_by_label(&format!("{words} · "));
        assert!(
            option(&w, "Save cleaned files", "In a folder…")
                .accesskit_node()
                .toggled()
                == Some(egui::accesskit::Toggled::True)
        );
        // F19, from the keyboard: the arrows onto it open nothing
        let mut w = files_window(files_state(None, false));
        answer_next(vec![dir.clone()]);
        option(&w, "Save cleaned files", "Next to the original").focus();
        w.harness.run_steps(2);
        w.harness.key_press(egui::Key::ArrowRight);
        w.harness.run_steps(3);
        assert!(
            option(&w, "Save cleaned files", "In a folder…")
                .accesskit_node()
                .toggled()
                == Some(egui::accesskit::Toggled::True),
            "the arrow moved the choice"
        );
        assert!(w.link.sent().is_empty(), "{:?}", w.link.sent());
        assert!(w
            .harness
            .query_by_label("Choose a folder for cleaned files")
            .is_some());
        // A cancelled chooser changes nothing: still "Choose a folder…",
        // and Next to the original goes back.
        let mut w = files_window(files_state(None, false));
        answer_next(vec![]);
        option(&w, "Save cleaned files", "In a folder…").click();
        w.harness.run_steps(3);
        w.harness
            .get_by_label("Choose a folder for cleaned files")
            .click();
        w.harness.run_steps(3);
        assert!(w.link.sent().is_empty());
        assert!(w
            .harness
            .query_by_label("Choose a folder for cleaned files")
            .is_some());
        option(&w, "Save cleaned files", "Next to the original").click();
        w.harness.run_steps(3);
        assert!(
            option(&w, "Save cleaned files", "Next to the original")
                .accesskit_node()
                .toggled()
                == Some(egui::accesskit::Toggled::True)
        );
        assert!(w
            .harness
            .query_by_label("Choose a folder for cleaned files")
            .is_none());
        // In a folder: Change… picks another; Next to the original goes back.
        let mut w = files_window(files_state(Some("/home/a/Recordings/Cleaned"), false));
        w.harness.get_by_label("Recordings/Cleaned · ");
        answer_next(vec![dir.clone()]);
        w.harness
            .get_by_label("Change the folder for cleaned files")
            .click();
        w.harness.run_steps(3);
        option(&w, "Save cleaned files", "Next to the original").click();
        w.harness.run_steps(3);
        assert_eq!(
            w.link.sent(),
            vec![Action::FilesFolder(dir), Action::FilesNextToOriginal]
        );
    }

    #[test]
    fn the_flatpak_shows_its_folder_and_changes_it() {
        use crate::pages::files::desktop::answer_next;
        let dir = std::env::temp_dir();
        let mut w = files_window(files_state(None, true));
        // no "next to the original" in a sandbox
        assert!(w.harness.query_by_label("Next to the original").is_none());
        w.harness.get_by_label("Not chosen yet · ");
        answer_next(vec![dir.clone()]);
        w.harness
            .get_by_label("Choose a folder for cleaned files")
            .click();
        w.harness.run_steps(3);
        assert_eq!(w.link.sent(), vec![Action::FilesFolder(dir)]);
        let w = files_window(files_state(Some("/home/a/Recordings/Cleaned"), true));
        w.harness.get_by_label("Recordings/Cleaned · ");
    }

    #[test]
    fn the_licenses_read_from_the_keyboard() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::NodeT;
        let mut w = testing::scenario(Scenario::OnInCall, Page::About);
        testing::by_role(&w.harness, "Licenses", Role::Link).click();
        w.harness.run_steps(3);
        let on_bar = |w: &testing::Window| {
            w.harness
                .query_by(|n| n.is_focused() && n.role() == Role::ScrollBar)
                .is_some()
        };
        for _ in 0..10 {
            if on_bar(&w) {
                break;
            }
            w.harness.key_press(egui::Key::Tab);
            w.harness.run_steps(2);
            // every stop on the way is named
            let unnamed = w
                .harness
                .query_by(|n| n.is_focused() && n.label().is_none_or(|l| l.is_empty()))
                .map(|n| format!("{:?}", n.accesskit_node().role()));
            assert_eq!(unnamed, None);
        }
        assert!(on_bar(&w), "the sheet's scroll bar is a Tab stop");
        // only what is in view is placed: the last crates come at the end
        let group = crate::ui::licenses::GROUPS.last().unwrap();
        let last = group.texts.last().unwrap();
        assert!(w.harness.query_by_label(last).is_none());
        w.harness.key_press(egui::Key::End);
        w.harness.run_steps(3);
        assert!(
            w.harness.query_by_label(last).is_some(),
            "{}",
            group.names()
        );
        let bar = testing::by_role(&w.harness, "Licenses", Role::ScrollBar);
        let n = bar.accesskit_node();
        let (at, max) = (n.numeric_value().unwrap(), n.max_numeric_value().unwrap());
        assert!(max > 0.0 && at == max, "{at} of {max}");
        // closed, the keyboard is back on the link that opened it
        w.harness.key_press(egui::Key::Escape);
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label("Close").is_none());
        assert!(testing::by_role(&w.harness, "Licenses", Role::Link).is_focused());
    }

    #[test]
    fn licence_texts_wrap_at_spaces_by_their_characters() {
        assert_eq!(mono_wrap("aaa bbb ccc ddd", 8), "aaa bbb\nccc ddd");
        // a word longer than a line is cut; blank lines stay
        assert_eq!(mono_wrap("abcdefghijk\n\nx", 8), "abcdefgh\nijk\n\nx");
        assert_eq!(mono_wrap("ä ö ü ä ö ü ä ö ü", 8), "ä ö ü ä\nö ü ä ö\nü");
    }

    #[test]
    fn appearance_turns_the_window_light_and_back() {
        let mut w = testing::mock_window(
            state(Scenario::OnInCall),
            Page::Settings,
            egui::vec2(960.0, 2000.0),
        );
        assert!(crate::ui::kit(&w.harness.ctx).dark());
        option(&w, "Appearance", "Light").click();
        w.harness.run_steps(3);
        assert!(!crate::ui::kit(&w.harness.ctx).dark());
        w.harness.get_by_label("System").click();
        w.harness.run_steps(3);
        assert!(crate::ui::kit(&w.harness.ctx).dark());
    }

    #[test]
    fn the_tray_icon_choice_is_off_while_the_tray_is() {
        let mut s = state(Scenario::OnInCall);
        s.prefs.tray = false;
        let mut w = testing::window(s, Page::Settings, egui::vec2(960.0, 2000.0));
        w.harness.get_by_label("Monochrome").click();
        w.harness.run_steps(2);
        assert!(w.link.sent().is_empty());
    }

    #[test]
    fn the_default_microphone_is_in_settings_apps() {
        let mut s = state(Scenario::OnInCall);
        s.apps.set_default = false;
        let mut w = testing::window(s.clone(), Page::Settings, egui::vec2(960.0, 2000.0));
        assert!(w.harness.query_by_label("Apps").is_some());
        w.harness
            .get_by_label("Use HushMic as the default microphone")
            .click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetDefaultMic(true)]);
        // the user moved it away: the setting stays on, the note asks
        s.apps.set_default = true;
        s.apps.default_released = Some("Built-in Audio".into());
        let mut w = testing::window(s.clone(), Page::Settings, egui::vec2(960.0, 2000.0));
        assert!(w
            .harness
            .query_by_label_contains("Apps now use Built-in Audio directly.")
            .is_some());
        w.harness.get_by_label("Make it the default again").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetDefaultMic(true)]);
        // where HushMic can't become the default, no Apps section
        s.apps.can_set_default = false;
        let w = testing::window(s, Page::Settings, egui::vec2(960.0, 2000.0));
        assert!(w
            .harness
            .query_by_label("Use HushMic as the default microphone")
            .is_none());
        // and Live no longer has it
        let w = testing::scenario(Scenario::OnInCall, Page::Live);
        assert!(w
            .harness
            .query_by_label_contains("default microphone")
            .is_none());
    }

    #[test]
    fn a_microphone_resets() {
        let mut w = testing::window(
            state(Scenario::OnInCall),
            Page::Settings,
            egui::vec2(960.0, 2000.0),
        );
        // round 3b: no hidden notices any more (the warning they hid went)
        assert!(w.harness.query_by_label_contains("Show again").is_none());
        w.harness.get_by_label("Reset RODE NT-USB").click();
        w.harness.run_steps(2);
        let rode = state(Scenario::OnInCall).sound.profiles[0].device.clone();
        assert_eq!(w.link.sent(), vec![Action::ResetProfile(rode)]);
    }

    #[test]
    fn the_microphone_in_use_is_listed_by_name() {
        crate::i18n::pin_english();
        // following the default onto a mic with no settings of its own:
        // it is listed by name, in use, before "Other microphones"
        let mut s = state(Scenario::OnInCall);
        let owner = s.mic.devices.iter().find(|d| d.present).unwrap().clone();
        s.sound.profiles.clear();
        s.sound.owner = Some(owner.name.clone());
        s.sound.saved = false;
        let w = testing::window(s.clone(), Page::Settings, egui::vec2(960.0, 2000.0));
        let words = format!("{} · in use", sound_words(&s.sound.current));
        assert!(w.harness.query_by_label(&owner.description).is_some());
        assert!(w.harness.query_by_label(&words).is_some(), "{words}");
        assert!(w.harness.query_by_label("Other microphones").is_some());
        // its settings saved: its own row says in use
        let mut s = state(Scenario::OnInCall);
        let p = s.sound.profiles[0].clone();
        s.sound.owner = Some(p.device.clone());
        s.sound.saved = true;
        let w = testing::window(s, Page::Settings, egui::vec2(960.0, 2000.0));
        let words = format!("{} · in use", sound_words(&p.sound));
        assert!(w.harness.query_by_label(&words).is_some(), "{words}");
        assert_eq!(
            w.harness.query_all_by_label(&p.description).count(),
            1,
            "listed once"
        );
    }

    #[test]
    fn two_of_a_name_stay_apart() {
        use egui_kittest::kittest::NodeT;
        // two microphones of one name (the snapshot numbers them; a row
        // goes by its device either way)
        let mut s = state(Scenario::OnInCall);
        let mut twin = s.sound.profiles[0].clone();
        twin.device = "alsa_input.usb-RODE_NT-USB-01.analog-stereo".into();
        s.sound.profiles.push(twin.clone());
        let mut w = testing::window(s, Page::Settings, egui::vec2(960.0, 2000.0));
        let resets: Vec<_> = w
            .harness
            .query_all_by_label("Reset RODE NT-USB")
            .map(|n| n.accesskit_node().id())
            .collect();
        assert_eq!(resets.len(), 2, "one Reset per microphone");
        w.harness
            .query_all_by_label("Reset RODE NT-USB")
            .find(|n| n.accesskit_node().id() == resets[1])
            .unwrap()
            .click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::ResetProfile(twin.device)]);
    }

    #[test]
    fn the_credits_read_as_one_sentence() {
        // compact: the credits wrap over several lines
        let w = testing::compact(Scenario::OnInCall, Page::About);
        let credits = tr!("set-credits");
        assert!(w.harness.query_by_label(&credits).is_some());
    }

    #[test]
    fn about_copies_diagnostics_and_shows_the_licences() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::About);
        assert_eq!(*w.page.borrow(), Page::Settings);
        // Waited for by what shows, not by a count of frames or a clock:
        // the copy and the sheet take their time on a loaded runner.
        let geist = "Geist and Geist Mono (SIL Open Font License 1.1)";
        let until = |w: &mut testing::Window, what: &str, f: &dyn Fn(&testing::Window) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            while !f(w) {
                assert!(std::time::Instant::now() < deadline, "{what}");
                w.harness.run_steps(1);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        // The report lands on the clipboard in one frame, and that frame's
        // button says so ("Copied" lasts 2 s, a frame on a loaded runner
        // can take longer).
        w.harness.get_by_label("Copy diagnostics").click();
        let copied = |w: &testing::Window| {
            w.harness
                .output()
                .platform_output
                .commands
                .iter()
                .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if !t.is_empty()))
        };
        until(&mut w, "the copy", &copied);
        assert!(w.harness.query_by_label("Copied").is_some());
        // Licenses sits in the copy button's row, and the click lands at
        // where it was drawn: wait out "Copied", whose end moves the row
        // (a loaded runner's next frame can come after it).
        until(&mut w, "the copy button back", &|w| {
            w.harness.query_by_label("Copy diagnostics").is_some()
        });
        w.harness.get_by_label("Licenses").click();
        until(&mut w, "the licences", &|w| {
            w.harness.query_by_label(geist).is_some()
        });
        w.harness.get_by_label("Close").click();
        until(&mut w, "the licences closed", &|w| {
            w.harness.query_by_label(geist).is_none()
        });
    }

    #[test]
    fn no_tray_says_how_to_get_one() {
        let w = testing::scenario(Scenario::NoTray, Page::Settings);
        assert!(w
            .harness
            .query_by_label_contains("No tray on this desktop.")
            .is_some());
    }
}
