//! Dev tool (pages parity): the accessibility tree of one screenshot scene
//! as JSON lines (role, name, rect), for comparing positions with the
//! boards' measured text.
//!
//! usage: pages-geom SCENE THEME WxH [key=value …]

use eframe::egui;
use egui_kittest::kittest::NodeT;
use hushmic::ui::paint::light::LightLayer;
use hushmic::ui::shot::{registry, Job};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut words = args.clone();
    words.insert(3, "/dev/null".into());
    let job = Job::parse(&words.join(" ")).unwrap_or_else(|e| {
        eprintln!("pages-geom: {e}");
        std::process::exit(2)
    });
    let def = registry()
        .into_iter()
        .find(|d| d.name == job.scene)
        .expect("unknown scene");
    let mut scene = None;
    let a = job.args.clone();
    let mut h = egui_kittest::Harness::builder()
        .with_size(job.args.size)
        .build(move |ctx| {
            let Some(s) = scene.as_mut() else {
                hushmic::ui::install(ctx, a.theme);
                scene = Some((def.make)(&a));
                ctx.request_repaint();
                return;
            };
            let mut light = LightLayer::new();
            egui::CentralPanel::default()
                .frame(egui::Frame::new())
                .show(ctx, |_| {});
            s.ui(ctx, &mut light);
        });
    h.run_steps(6);
    for n in h.root().children_recursive() {
        let ak = n.accesskit_node();
        let name = ak.label().unwrap_or_default();
        let value = ak.value().unwrap_or_default();
        if name.is_empty() && value.is_empty() {
            continue;
        }
        // some nodes carry no bounds of their own (a group's parts)
        let Some(r) = ak.bounding_box() else {
            continue;
        };
        println!(
            "{}",
            serde_json::json!({"role": format!("{:?}", ak.role()), "t": name, "v": value,
                "x": r.x0, "y": r.y0, "w": r.width(), "h": r.height()})
        );
    }
}
