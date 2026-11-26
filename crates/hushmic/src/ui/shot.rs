//! The screenshot harness: renders any registered scene (a page in a state,
//! a gallery sheet, a painter frozen at a moment) in a real eframe window
//! (glow, the app's renderer) and writes PNGs. It runs under Xvfb for the
//! parity loop (`scripts/ui-parity/`); the scale comes from the X server
//! (`WINIT_X11_SCALE_FACTOR=2` for 2x).
//!
//! A scene is deterministic: no wall-clock input, live levels prefilled from
//! the demo clips, moments frozen with `scrub`. Pages add their scenes to
//! [`registry`].

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Context, Vec2};

use super::paint::light::LightLayer;
use super::tokens::Theme;

/// What a scene is built from: theme, window size, and free `key=value`
/// parameters (state names, column counts, moment times…).
#[derive(Clone, Debug)]
pub struct SceneArgs {
    pub theme: Theme,
    pub size: Vec2,
    pub params: HashMap<String, String>,
}

impl SceneArgs {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(String::as_str)
    }
    pub fn num(&self, key: &str, default: f32) -> f32 {
        self.get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some("1" | "true" | "yes" | "on"))
    }
    /// The boards' "Minimum 720 × 520": the compact layout.
    pub fn compact(&self) -> bool {
        self.size.x < super::tokens::window::COMPACT_BELOW
    }
}

pub trait Scene {
    /// Draw one frame of the whole window. Anything handed to `light` is
    /// painted over the window afterwards.
    fn ui(&mut self, ctx: &Context, light: &mut LightLayer);
}

pub struct SceneDef {
    pub name: &'static str,
    pub about: &'static str,
    pub make: fn(&SceneArgs) -> Box<dyn Scene>,
}

/// Every scene the harness knows. Pages append theirs here.
pub fn registry() -> Vec<SceneDef> {
    let mut v = super::gallery::scenes();
    v.extend(super::replica::scenes());
    v.extend(crate::pages::scenes::scenes());
    v.sort_by_key(|s| s.name);
    v
}

/// One screenshot to take.
#[derive(Clone, Debug)]
pub struct Job {
    pub scene: String,
    pub args: SceneArgs,
    pub out: PathBuf,
}

impl Job {
    /// `scene theme WxH out.png [key=value …]`
    pub fn parse(line: &str) -> Result<Job, String> {
        let mut it = line.split_whitespace();
        let scene = it.next().ok_or("empty job")?.to_owned();
        let theme = match it.next().ok_or("job needs a theme")? {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            t => return Err(format!("unknown theme {t}")),
        };
        let size = it.next().ok_or("job needs a size")?;
        let (w, h) = size.split_once('x').ok_or("size is WxH")?;
        let size = egui::vec2(
            w.parse().map_err(|_| "bad width")?,
            h.parse().map_err(|_| "bad height")?,
        );
        let out = PathBuf::from(it.next().ok_or("job needs an output file")?);
        let mut params = HashMap::new();
        for kv in it {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| format!("bad parameter {kv}"))?;
            params.insert(k.to_owned(), v.to_owned());
        }
        Ok(Job {
            scene,
            args: SceneArgs {
                theme,
                size,
                params,
            },
            out,
        })
    }
}

enum Phase {
    Start,
    Resize(u32),
    Settle(u32),
    Shoot,
    /// After the last shot: keep the scene up until this time.
    Hold(f64),
}

struct Runner {
    jobs: Vec<Job>,
    defs: Vec<SceneDef>,
    i: usize,
    phase: Phase,
    scene: Option<Box<dyn Scene>>,
    software: bool,
    hold: f64,
    errors: Vec<String>,
}

fn write_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(f),
        img.size[0] as u32,
        img.size[1] as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    w.write_image_data(&bytes).map_err(|e| e.to_string())
}

impl eframe::App for Runner {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        super::begin_frame(ctx);
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let job = &self.jobs[self.i];
            match write_png(&job.out, &img) {
                Ok(()) => println!(
                    "wrote {} {}x{}",
                    job.out.display(),
                    img.size[0],
                    img.size[1]
                ),
                Err(e) => self.errors.push(format!("{}: {e}", job.out.display())),
            }
            if self.i + 1 == self.jobs.len() && self.hold > 0.0 {
                self.phase = Phase::Hold(ctx.input(|i| i.time) + self.hold);
            } else {
                self.i += 1;
                self.phase = Phase::Start;
                self.scene = None;
            }
        }
        if self.i >= self.jobs.len() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let job = self.jobs[self.i].clone();
        if matches!(self.phase, Phase::Start) {
            super::set_theme(ctx, job.args.theme);
            super::set_software(ctx, self.software);
            match self.defs.iter().find(|d| d.name == job.scene) {
                Some(d) => self.scene = Some((d.make)(&job.args)),
                None => {
                    self.errors.push(format!("unknown scene {}", job.scene));
                    self.i += 1;
                    ctx.request_repaint();
                    return;
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(job.args.size));
            self.phase = Phase::Resize(0);
        }
        let size = ctx.content_rect().size();
        let pal = super::kit(ctx).pal;
        let mut light = LightLayer::new();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg))
            .show(ctx, |_ui| {});
        if let Some(scene) = self.scene.as_mut() {
            scene.ui(ctx, &mut light);
        }
        light.paint(ctx, ctx.content_rect());
        self.phase = match self.phase {
            Phase::Resize(_) if (size - job.args.size).length() < 0.5 => Phase::Settle(0),
            Phase::Resize(n) if n > 120 => {
                self.errors
                    .push(format!("{}: window stayed at {size:?}", job.out.display()));
                Phase::Settle(0)
            }
            Phase::Resize(n) => Phase::Resize(n + 1),
            Phase::Settle(n) if n >= 3 => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                Phase::Shoot
            }
            Phase::Settle(n) => Phase::Settle(n + 1),
            Phase::Shoot => Phase::Shoot,
            Phase::Start => Phase::Start,
            Phase::Hold(until) => {
                if ctx.input(|i| i.time) >= until {
                    self.i += 1;
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
        };
        ctx.request_repaint();
    }
}

/// Take the screenshots. `software` draws the software-renderer guard look;
/// `hold` keeps the last scene open that many seconds (AT-SPI checks, a look).
pub fn run(jobs: Vec<Job>, software: bool, hold: f64) -> Result<(), String> {
    if jobs.is_empty() {
        return Ok(());
    }
    let first = jobs[0].args.size;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HushMic")
            .with_app_id("hushmic-shot")
            .with_inner_size(first)
            .with_resizable(true),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let errs = errors.clone();
    eframe::run_native(
        "hushmic-shot",
        options,
        Box::new(move |cc| {
            super::install(&cc.egui_ctx, Theme::Dark);
            struct Wrap(Runner, std::sync::Arc<std::sync::Mutex<Vec<String>>>);
            impl eframe::App for Wrap {
                // shots never see the pointer: under Xvfb it sits mid-screen
                // and would hover whatever lands there. A held window (after
                // its shot) takes it, for hover and click checks.
                fn raw_input_hook(&mut self, _: &Context, raw: &mut egui::RawInput) {
                    if matches!(self.0.phase, Phase::Hold(_)) {
                        return;
                    }
                    raw.events.retain(|e| {
                        !matches!(
                            e,
                            egui::Event::PointerMoved(_)
                                | egui::Event::MouseMoved(_)
                                | egui::Event::PointerButton { .. }
                        )
                    });
                }
                fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
                    self.0.update(ctx, frame);
                    if !self.0.errors.is_empty() {
                        let mut e = self.1.lock().unwrap_or_else(|e| e.into_inner());
                        e.append(&mut self.0.errors);
                    }
                }
            }
            Ok(Box::new(Wrap(
                Runner {
                    jobs,
                    defs: registry(),
                    i: 0,
                    phase: Phase::Start,
                    scene: None,
                    software,
                    hold,
                    errors: Vec::new(),
                },
                errs,
            )))
        }),
    )
    .map_err(|e| e.to_string())?;
    let errors = errors.lock().unwrap_or_else(|e| e.into_inner());
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_parse() {
        let j = Job::parse("gallery/controls light 720x520 out/a.png state=on cols=180").unwrap();
        assert_eq!(j.scene, "gallery/controls");
        assert_eq!(j.args.theme, Theme::Light);
        assert_eq!(j.args.size, egui::vec2(720.0, 520.0));
        assert!(j.args.compact());
        assert_eq!(j.args.num("cols", 0.0), 180.0);
        assert!(Job::parse("x dusk 1x1 a.png").is_err());
    }

    #[test]
    fn every_scene_draws_headless() {
        // each registered scene runs a few frames in a headless context
        // without panicking, in both themes and both sizes
        for def in registry() {
            for theme in [Theme::Dark, Theme::Light] {
                for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
                    let ctx = Context::default();
                    super::super::install(&ctx, theme);
                    let args = SceneArgs {
                        theme,
                        size,
                        params: HashMap::new(),
                    };
                    let mut scene = (def.make)(&args);
                    for _ in 0..3 {
                        let input = egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            ..Default::default()
                        };
                        let _ = ctx.run(input, |ctx| {
                            let mut light = LightLayer::new();
                            egui::CentralPanel::default().show(ctx, |_| {});
                            scene.ui(ctx, &mut light);
                            light.paint(ctx, ctx.content_rect());
                        });
                    }
                }
            }
        }
    }
}
