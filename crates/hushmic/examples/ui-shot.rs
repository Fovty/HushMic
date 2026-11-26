//! Screenshots of the window's scenes (dev tool, see scripts/ui-parity/).
//!
//! usage: ui-shot --list
//!        ui-shot [--software] [--hold SECS] SCENE THEME WxH OUT.png [key=value …]
//!        ui-shot [--software] [--hold SECS] --jobs FILE   (one job per line)
//! Run it under Xvfb; WINIT_X11_SCALE_FACTOR=2 gives the 2x shots. Shots never
//! see the pointer. --hold keeps the last scene open after its shot (an AT-SPI
//! check, a look, hover and click tests: the held window takes the pointer).

use hushmic::ui::shot::{registry, run, Job};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--list") {
        for s in registry() {
            println!("{:<22} {}", s.name, s.about);
        }
        return;
    }
    let mut software = false;
    let mut hold = 0.0;
    loop {
        match args.first().map(String::as_str) {
            Some("--software") => {
                args.remove(0);
                software = true;
            }
            Some("--hold") if args.len() > 1 => {
                hold = args[1].parse().unwrap_or(0.0);
                args.drain(..2);
            }
            _ => break,
        }
    }
    let jobs: Result<Vec<Job>, String> = if args.first().map(String::as_str) == Some("--jobs") {
        let file = args.get(1).cloned().unwrap_or_default();
        match std::fs::read_to_string(&file) {
            Ok(text) => text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(Job::parse)
                .collect(),
            Err(e) => Err(format!("{file}: {e}")),
        }
    } else {
        Job::parse(&args.join(" ")).map(|j| vec![j])
    };
    let result = jobs.and_then(|jobs| run(jobs, software, hold));
    if let Err(e) = result {
        eprintln!("ui-shot: {e}");
        std::process::exit(1);
    }
}
