//! The pages as screenshot scenes (`ui-shot`, `scripts/ui-parity/`): every
//! board state, built from the mock daemon states. Live's picture shows a
//! synthetic take ([`crate::miccheck::demo`]), the same in every shot;
//! first run's columns are prefilled from the boards' keyboard clip so a
//! shot matches the board's same state.
//!
//! `page/live state=on-no-app apps=none replay=result cols=200` ·
//! `page/shortcuts state=keys-not-reported` · `page/settings state=no-tray
//! view=about` · `page/first-run state=first-run-2 mode=bypass` ·
//! `page/speakers speakers=playing` (`state=off`: HushMic off).

use eframe::egui;

use super::replay::Preset;
use super::{Feed, Pages};
use crate::appstate::mock::{self, Scenario};
use crate::appstate::{AppState, Mode, Page};
use crate::ui::paint::light::LightLayer;
use crate::ui::shot::{Scene, SceneArgs, SceneDef};
use crate::window::Link;

pub fn scenes() -> Vec<SceneDef> {
    vec![
        SceneDef {
            name: "page/live",
            about: "Live: state=<mock scenario> strength=gentle|medium|strong|maximum apps=none|anon|Zoom,OBS_Studio replay=replaying|holding|latched|result|sequential|quiet|loud|silent|changed|unavailable cols=N quit=1 more=1 pick=open mic=default|long",
            make: |a| Box::new(PageScene::new(a, Page::Live)),
        },
        #[cfg(feature = "speakers")]
        SceneDef {
            name: "page/speakers",
            about: "Speakers: speakers=off|starting|idle|playing|held|unplugged|down|light|is-default|left state=<mock scenario> (no board)",
            make: |a| Box::new(PageScene::new(a, Page::Speakers)),
        },
        SceneDef { name: "page/files", about: "Files: state=<App-Files2 state> [around=quit] (App-Files2)", make: |a| Box::new(PageScene::new(a, Page::Files)) },
        SceneDef { name: "page/shortcuts", about: "Shortcuts: state=<mock scenario> (App-Shortcuts)", make: |a| Box::new(PageScene::new(a, Page::Shortcuts)) },
        SceneDef {
            name: "page/settings",
            about: "Settings: state=<mock scenario> view=top|about|files files=next|folder|flatpak|flatpak-unset (App-Settings; App-Files around: Settings › Files)",
            make: |a| Box::new(PageScene::new(a, Page::Settings)),
        },
        SceneDef {
            name: "page/first-run",
            about: "First run: state=first-run-1|2|3|… mode=filtering|bypass|muted|off cols=N (App-FirstRun)",
            make: |a| Box::new(PageScene::new(a, Page::FirstRun)),
        },
    ]
}

struct PageScene {
    pages: Option<Pages>,
    link: Link,
    page: Page,
    args: SceneArgs,
}

fn scenario(a: &SceneArgs, page: Page) -> (AppState, Option<Preset>) {
    let word = a.get("state").unwrap_or(match page {
        Page::FirstRun => "first-run-1",
        _ => "on-in-call",
    });
    let mut s = mock::state(Scenario::parse(word).unwrap_or(Scenario::OnInCall));
    // Live's last 10 seconds and the level, as App-Live2's state picks them
    let preset = a.get("replay").and_then(Preset::parse);
    // who records HushMic (App-Live2's `apps`): names ("_" for a space),
    // "anon" (one app with no name) or "none"
    match a.get("apps") {
        Some("none") => s.apps.using.clear(),
        Some("anon") => {
            s.apps.using = vec![crate::appstate::AppRef {
                id: "app".into(),
                name: None,
            }]
        }
        Some(names) => {
            s.apps.using = names
                .split(',')
                .map(|n| crate::appstate::AppRef {
                    id: n.to_lowercase(),
                    name: Some(n.replace('_', " ")),
                })
                .collect()
        }
        None => {}
    }
    match a.get("mode") {
        Some("bypass") => {
            s.mode = Mode::Bypass;
            s.unmute_to = Mode::Bypass;
        }
        Some("muted") => s.mode = Mode::Mute,
        Some("off") => {
            s.mode = Mode::Off;
            s.engine = crate::appstate::Engine::default();
        }
        _ => {}
    }
    // the microphone's strength (Live): gentle|medium|strong|maximum
    if let Some(st) = a.get("strength").and_then(|w| {
        crate::appstate::Strength::PRESETS
            .into_iter()
            .find(|(st, _)| format!("{st:?}").eq_ignore_ascii_case(w))
    }) {
        let model = s.sound.current.model_id.clone();
        s.sound.current = crate::appstate::Sound::new(&model, st.1);
    }
    // the speakers cleaner's state (the Speakers page)
    #[cfg(feature = "speakers")]
    if let Some(sc) = a.get("speakers").and_then(mock::SpeakersScene::parse) {
        s = mock::with_speakers(s, sc);
    }
    if a.get("view") == Some("about") {
        s.ui.page = Page::About;
    }
    // pick=open: one of the microphones has a long name
    if a.get("pick") == Some("open") {
        if let Some(d) = s.mic.devices.last_mut() {
            d.description =
                "Jabra Evolve2 85 Wireless Headset with Noise Cancelling Boom Microphone Mono"
                    .into();
        }
    }
    // mic=long: the microphone with the long name is the chosen one
    if a.get("mic") == Some("long") {
        s.mic.selected = s.mic.devices.last().map(|d| d.name.clone());
        s.mic.active = s.mic.selected.clone();
    }
    // no microphone chosen: HushMic follows the system default's device
    if a.get("mic") == Some("default") {
        s.mic.selected = None;
        s.mic.active = s.mic.system_default.clone();
    }
    // Status problems with no state of their own: another app takes the
    // microphone; the chosen one isn't connected (no stand-in)
    if a.get("rerouted").is_some() {
        s.mic.rerouted = true;
    }
    if a.get("absent").is_some() {
        let chosen = s.mic.selected.clone();
        for d in s.mic.devices.iter_mut() {
            if Some(&d.name) == chosen.as_ref() {
                d.present = false;
            }
        }
    }
    // not on the boards (appux §9): the chain keeps restarting
    if a.get("flapping").is_some() {
        s.engine.flapping = true;
    }
    // App-Files "around: Settings › Files": where Save puts cleaned files
    match a.get("files") {
        Some("folder") => s.files.folder = Some("/home/you/Recordings/Cleaned".into()),
        Some("flatpak") => {
            s.files.needs_folder = true;
            s.files.folder = Some("/home/you/Recordings/Cleaned".into());
        }
        Some("flatpak-unset") => s.files.needs_folder = true,
        _ => {}
    }
    // App-Files "around: Quit while cleaning": two files the question stops
    if a.get("around") == Some("quit") {
        use crate::appstate::{FileJob, JobState};
        s.files.ask_quit = true;
        s.files.jobs = [JobState::Cleaning, JobState::Waiting]
            .into_iter()
            .zip(1..)
            .map(|(state, id)| FileJob {
                id,
                state,
                ..FileJob::default()
            })
            .collect();
    }
    (s, preset)
}

impl PageScene {
    fn new(a: &SceneArgs, page: Page) -> PageScene {
        let (state, _) = scenario(a, page);
        let page = if state.ui.page == Page::About {
            Page::About
        } else {
            page
        };
        PageScene {
            pages: None,
            link: Link::recording(state),
            page,
            args: a.clone(),
        }
    }
}

impl Scene for PageScene {
    fn ui(&mut self, ctx: &egui::Context, _light: &mut LightLayer) {
        let a = &self.args;
        let page = self.page;
        let pages = self.pages.get_or_insert_with(|| {
            let mut p = Pages::with_kit(Feed::Demo);
            p.pin_theme(a.theme);
            if a.get("launch").is_none() {
                p.no_launch();
            }
            let (_, preset) = scenario(a, page);
            p.live.replay.preset = preset;
            if page == Page::Files {
                // board-shot captures 3 s after the page loaded, about
                // 3.3 s into the state
                let state = a.get("state").unwrap_or("empty").to_owned();
                p.files.preset = Some((state, f64::from(a.num("secs", 3.3))));
            }
            // Live's picture holds 10 s (200 columns): full by default
            let full = if page == Page::Live { 200.0 } else { 180.0 };
            let cols = a.num("cols", full) as usize;
            if a.get("state") == Some("mic-silent") {
                p.live.silent_demo();
            }
            // pick=open: the microphone's list open over the picture
            if a.get("pick") == Some("open") {
                p.live.open_pick();
            }
            if a.get("view") == Some("files") {
                p.settings_on_files();
            }
            p.prefill(cols);
            p.set_diagnostics(String::new);
            // quit=1: More › Quit HushMic was chosen (the sheet while an app
            // uses HushMic)
            if a.flag("quit") {
                p.ask_quit(&self.link);
            }
            // more=1: the rail's More menu open
            if a.flag("more") {
                p.open_more();
            }
            p
        });
        pages.show(ctx, &self.link, &mut self.page);
    }
}
