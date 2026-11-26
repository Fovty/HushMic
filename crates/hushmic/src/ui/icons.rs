//! Painted line icons (Lucide geometry, ISC: `licenses/ISC-Lucide.txt`).
//!
//! The boards draw their icons as SVG on a 24-unit grid with a 1.5-unit
//! stroke, round caps and joins; at 16 px that is a 1 px line. egui paints
//! the same paths: parsed once, flattened to polylines, stroked at the
//! board's width. Nothing to rasterise, crisp at every scale.

use std::collections::HashMap;
use std::sync::OnceLock;

use eframe::egui::{pos2, vec2, Color32, Mesh, Painter, Pos2, Shape, Stroke};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Rail: Live.
    Live,
    /// Headphones (once the rail's Mic check).
    Check,
    /// Rail: Files (a file with sound bars).
    Files,
    /// Rail: Shortcuts.
    Keys,
    /// Rail: Settings.
    Settings,
    /// The Mute button.
    Mic,
    /// The Unmute button.
    MicOff,
    Chevron,
    More,
    App,
    Warn,
    Info,
    Plug,
    Power,
    Quit,
    Replay,
    Play,
    Pause,
    Copy,
    External,
    Tick,
    Close,
    Minimize,
    Maximize,
    /// Stop (a replay): a square.
    Stop,
    Hold,
    Speaker,
    /// The Live picture's mic end (Lucide `mic`, drawn at 20 px, 1.5 px).
    MicGlyph,
    /// The Live picture's app end (Lucide `app-window`).
    AppGlyph,
    /// Before the original's name over Files' picture: a file with sound
    /// bars, a file with a play mark (Lucide `file`, drawn at 20 px,
    /// 1.5 px).
    FileSound,
    FileFilm,
    /// Files' rows: a sound file, a video file, any other file (Lucide
    /// `file`).
    Sound,
    Film,
    File,
    /// Show in folder.
    Folder,
    /// A row's Save (Lucide `save`).
    Save,
    /// Files' Add files….
    Plus,
}

enum Prim {
    Path(&'static str),
    Circle(f32, f32, f32),
    Rect(f32, f32, f32, f32, f32),
}
use Prim::*;

fn prims(icon: Icon) -> &'static [Prim] {
    match icon {
        Icon::Live => &[Path("M2 12h3l3-8 4 16 3-8h7")],
        Icon::Check => &[
            Path("M3 18v-6a9 9 0 0 1 18 0v6"),
            Rect(3.0, 15.0, 4.0, 6.0, 1.0),
            Rect(17.0, 15.0, 4.0, 6.0, 1.0),
        ],
        Icon::Files => &[
            Path("M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"),
            Path("M14 3v5h5"),
            Path("M9 13v4M12 11.5v7M15 13.5v3"),
        ],
        Icon::FileSound => &[
            Path("M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"),
            Path("M14 2v4a2 2 0 0 0 2 2h4"),
            Path("M9 14v4"),
            Path("M12 12v7"),
            Path("M15 15v2"),
        ],
        Icon::FileFilm => &[
            Path("M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"),
            Path("M14 2v4a2 2 0 0 0 2 2h4"),
            Path("M10 12.5v5l4-2.5Z"),
        ],
        Icon::Sound => &[Path("M4 10v4M8 7v10M12 4v16M16 8v8M20 11v2")],
        Icon::Film => &[
            Rect(3.0, 5.0, 18.0, 14.0, 2.0),
            Path("M7 5v14M17 5v14M3 9.5h4M3 14.5h4M17 9.5h4M17 14.5h4"),
        ],
        Icon::Folder => &[Path("M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z")],
        Icon::Plus => &[Path("M12 5v14M5 12h14")],
        Icon::Save => &[
            Path("M15.2 3a2 2 0 0 1 1.4 0.6l3.8 3.8a2 2 0 0 1 0.6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"),
            Path("M17 21v-7a1 1 0 0 0-1-1H8a1 1 0 0 0-1 1v7"),
            Path("M7 3v4a1 1 0 0 0 1 1h7"),
        ],
        Icon::File => &[
            Path("M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"),
            Path("M14 2v4a2 2 0 0 0 2 2h4"),
        ],
        Icon::Keys => &[Rect(2.0, 6.0, 20.0, 12.0, 2.0), Path("M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10")],
        Icon::Settings => &[
            Path("M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0"),
            Circle(16.0, 6.0, 2.0),
            Circle(10.0, 12.0, 2.0),
            Circle(18.0, 18.0, 2.0),
        ],
        Icon::Mic => &[Rect(9.0, 2.0, 6.0, 12.0, 3.0), Path("M19 10v1a7 7 0 0 1-14 0v-1"), Path("M12 18v4")],
        Icon::MicOff => &[
            Path("M2 2l20 20"),
            Path("M15 9.3V5a3 3 0 0 0-5.7-1.3"),
            Path("M9 9v2a3 3 0 0 0 5.1 2.1"),
            Path("M19 10v1a7 7 0 0 1-.6 2.9"),
            Path("M5 10v1a7 7 0 0 0 11.8 5.1"),
            Path("M12 18v4"),
        ],
        Icon::Chevron => &[Path("m6 9 6 6 6-6")],
        Icon::More => &[Circle(5.0, 12.0, 1.0), Circle(12.0, 12.0, 1.0), Circle(19.0, 12.0, 1.0)],
        Icon::App => &[Rect(3.0, 4.0, 18.0, 16.0, 2.0), Path("M3 9h18"), Path("M7 6.5h.01M10 6.5h.01")],
        Icon::Warn => &[
            Path("M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"),
            Path("M12 9v4"),
            Path("M12 17h.01"),
        ],
        Icon::Info => &[Circle(12.0, 12.0, 9.0), Path("M12 11v5"), Path("M12 8h.01")],
        Icon::Plug => &[Path("M9 2v6M15 2v6"), Path("M6 8h12v3a6 6 0 0 1-12 0z"), Path("M12 17v5")],
        Icon::Power => &[Path("M12 2v10"), Path("M18.4 6.6a9 9 0 1 1-12.8 0")],
        Icon::Quit => &[Path("M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4"), Path("M10 17l5-5-5-5"), Path("M15 12H3")],
        Icon::Replay => &[Path("M3 12a9 9 0 1 0 3-6.7L3 8"), Path("M3 3v5h5")],
        Icon::Play => &[Path("M7 4.5v15l12.5-7.5z")],
        Icon::Pause => &[Path("M7 5h3v14H7zM14 5h3v14h-3z")],
        Icon::Copy => &[Rect(8.0, 8.0, 13.0, 13.0, 2.0), Path("M4 16V5a2 2 0 0 1 2-2h11")],
        Icon::External => &[Path("M14 4h6v6"), Path("M10 14 20 4"), Path("M19 13v6a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1h6")],
        Icon::Tick => &[Path("M20 6 9 17l-5-5")],
        Icon::Close => &[Path("M6 6l12 12M18 6 6 18")],
        Icon::Minimize => &[Path("M6 12h12")],
        Icon::Maximize | Icon::Stop => &[Rect(6.0, 6.0, 12.0, 12.0, 1.0)],
        Icon::Hold => &[
            Path("M8 13V5.5a1.5 1.5 0 0 1 3 0V12"),
            Path("M11 11.5v-2a1.5 1.5 0 0 1 3 0V12"),
            Path("M14 10.5a1.5 1.5 0 0 1 3 0V12"),
            Path("M17 11.5a1.5 1.5 0 0 1 3 0V16a6 6 0 0 1-6 6h-2a6 6 0 0 1-5-2.7L4.3 15a1.5 1.5 0 0 1 2.4-1.8L8 15"),
        ],
        Icon::Speaker => &[Path("M11 5 6 9H2v6h4l5 4z"), Path("M15.5 8.5a5 5 0 0 1 0 7")],
        Icon::MicGlyph => &[
            Path("M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"),
            Path("M19 10v2a7 7 0 0 1-14 0v-2"),
            Path("M12 19v3"),
        ],
        Icon::AppGlyph => &[
            Path("M4 4h16a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z"),
            Path("M2 8h20"),
            Path("M6 4v4"),
            Path("M10 4v4"),
        ],
    }
}

/// One flattened subpath on the 24-unit grid.
#[derive(Clone, Debug)]
pub struct Sub {
    pub pts: Vec<Pos2>,
    pub closed: bool,
}

fn flatten(icon: Icon) -> Vec<Sub> {
    let mut out = Vec::new();
    for p in prims(icon) {
        match *p {
            Path(d) => out.extend(parse_path(d)),
            Circle(cx, cy, r) => {
                let n = 32;
                let pts = (0..n)
                    .map(|i| {
                        let a = std::f32::consts::TAU * i as f32 / n as f32;
                        pos2(cx + r * a.cos(), cy + r * a.sin())
                    })
                    .collect();
                out.push(Sub { pts, closed: true });
            }
            Rect(x, y, w, h, r) => {
                let r = r.min(w / 2.0).min(h / 2.0);
                let mut pts = Vec::new();
                let corner = |pts: &mut Vec<Pos2>, cx: f32, cy: f32, a0: f32| {
                    for i in 0..=6 {
                        let a = a0 + std::f32::consts::FRAC_PI_2 * i as f32 / 6.0;
                        pts.push(pos2(cx + r * a.cos(), cy + r * a.sin()));
                    }
                };
                use std::f32::consts::PI;
                corner(&mut pts, x + w - r, y + r, -PI / 2.0);
                corner(&mut pts, x + w - r, y + h - r, 0.0);
                corner(&mut pts, x + r, y + h - r, PI / 2.0);
                corner(&mut pts, x + r, y + r, PI);
                out.push(Sub { pts, closed: true });
            }
        }
    }
    out
}

fn cache() -> &'static std::sync::Mutex<HashMap<Icon, std::sync::Arc<Vec<Sub>>>> {
    static C: OnceLock<std::sync::Mutex<HashMap<Icon, std::sync::Arc<Vec<Sub>>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub fn subpaths(icon: Icon) -> std::sync::Arc<Vec<Sub>> {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.entry(icon)
        .or_insert_with(|| std::sync::Arc::new(flatten(icon)))
        .clone()
}

/// Paint `icon` centred on `center` in a `size` px box with the boards' UI
/// stroke (1.5 units on the 24 grid: 1 px at 16 px).
pub fn paint(painter: &Painter, icon: Icon, center: Pos2, size: f32, color: Color32) {
    paint_with(painter, icon, center, size, color, 1.5 * size / 24.0, None);
}

/// Paint with an explicit stroke width in points and optional dashes
/// (`[dash, gap]` in points along the path, as canvas `setLineDash`).
pub fn paint_with(
    painter: &Painter,
    icon: Icon,
    center: Pos2,
    size: f32,
    color: Color32,
    width: f32,
    dash: Option<[f32; 2]>,
) {
    let k = size / 24.0;
    let o = center - vec2(size, size) * 0.5;
    let stroke = Stroke::new(width, color);
    let faint = icon == Icon::Keys;
    for sub in subpaths(icon).iter() {
        let mut pts: Vec<Pos2> = sub.pts.iter().map(|p| o + p.to_vec2() * k).collect();
        if sub.closed {
            if let Some(first) = pts.first().copied() {
                pts.push(first);
            }
        }
        match dash {
            None => stroke_round(painter, pts, sub.closed, stroke, faint),
            Some([on, off]) => {
                for seg in dashes(&pts, on, off) {
                    stroke_round(painter, seg, false, stroke, faint);
                }
            }
        }
    }
}

/// Chrome draws strokes of at most one device pixel as hairlines: a dot
/// then is a one-pixel square in the dot's pixel row, shared by the pixels
/// it straddles (a lone dot: the info and warning icons). The keyboard's
/// keys, dots in one path with the space bar, come out at half that in
/// quarter steps on the boards.
fn hairline_dot(painter: &Painter, at: Pos2, color: Color32, faint: bool) {
    let ppp = painter.pixels_per_point();
    let (x, y) = (at.x * ppp, (at.y * ppp).floor());
    let mut mesh = Mesh::default();
    let left = (x - 0.5).floor();
    for px in [left, left + 1.0] {
        let share = ((px + 1.0).min(x + 0.5) - px.max(x - 0.5)).max(0.0);
        let k = if faint {
            (share * 0.5 * 4.0 + 0.002).floor() / 4.0
        } else {
            share
        };
        if k > 0.0 {
            let r = eframe::egui::Rect::from_min_size(
                pos2(px / ppp, y / ppp),
                vec2(1.0 / ppp, 1.0 / ppp),
            );
            mesh.add_colored_rect(r, color.gamma_multiply(k));
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// A polyline with round caps (egui ends strokes square; a disc at each
/// end gives the SVG `stroke-linecap: round`).
fn stroke_round(painter: &Painter, pts: Vec<Pos2>, closed: bool, stroke: Stroke, faint: bool) {
    if pts.is_empty() {
        return;
    }
    let len: f32 = pts.windows(2).map(|w| (w[1] - w[0]).length()).sum();
    if len < 0.05 {
        // a dot ("M7 6.5h.01"): a round dot from 2x
        let ppp = painter.pixels_per_point();
        if stroke.width * ppp > 1.001 {
            painter.circle_filled(pts[0], stroke.width / 2.0, stroke.color);
        } else {
            hairline_dot(painter, pts[0], stroke.color, faint);
        }
        return;
    }
    if !closed {
        let (a, b) = (pts[0], pts[pts.len() - 1]);
        painter.add(Shape::line(pts, stroke));
        painter.circle_filled(a, stroke.width / 2.0, stroke.color);
        painter.circle_filled(b, stroke.width / 2.0, stroke.color);
    } else {
        painter.add(Shape::line(pts, stroke));
    }
}

fn dashes(pts: &[Pos2], on: f32, off: f32) -> Vec<Vec<Pos2>> {
    // a dash or gap of nothing would never move along the path
    let (on, off) = (on.max(0.1), off.max(0.1));
    let mut out = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    let mut drawing = true;
    let mut left = on;
    for w in pts.windows(2) {
        let (mut a, b) = (w[0], w[1]);
        let mut seg = (b - a).length();
        while seg > 1e-4 {
            let step = seg.min(left);
            let p = a + (b - a).normalized() * step;
            if drawing {
                if cur.is_empty() {
                    cur.push(a);
                }
                cur.push(p);
            }
            left -= step;
            seg -= step;
            a = p;
            if left <= 1e-4 {
                if drawing && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                drawing = !drawing;
                left = if drawing { on } else { off };
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------- SVG paths

fn parse_path(d: &str) -> Vec<Sub> {
    let toks = tokenize(d);
    let mut subs: Vec<Sub> = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    let mut p = pos2(0.0, 0.0);
    let mut start = p;
    let mut last_ctrl: Option<Pos2> = None;
    let mut i = 0;
    let mut cmd = 'M';
    let num = |i: &mut usize| -> f32 {
        let v = match toks.get(*i) {
            Some(Tok::Num(v)) => *v,
            _ => 0.0,
        };
        *i += 1;
        v
    };
    let flush = |subs: &mut Vec<Sub>, cur: &mut Vec<Pos2>, closed: bool| {
        if cur.len() > 1 || (cur.len() == 1 && closed) {
            subs.push(Sub {
                pts: std::mem::take(cur),
                closed,
            });
        } else {
            cur.clear();
        }
    };
    while i < toks.len() {
        if let Tok::Cmd(c) = toks[i] {
            cmd = c;
            i += 1;
            if c == 'Z' || c == 'z' {
                flush(&mut subs, &mut cur, true);
                p = start;
                last_ctrl = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { p } else { pos2(0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            'M' => {
                flush(&mut subs, &mut cur, false);
                let x = num(&mut i);
                let y = num(&mut i);
                p = base + vec2(x, y);
                start = p;
                cur.push(p);
                // further pairs are implicit line-tos
                cmd = if rel { 'l' } else { 'L' };
                last_ctrl = None;
            }
            'L' => {
                let x = num(&mut i);
                let y = num(&mut i);
                p = base + vec2(x, y);
                cur.push(p);
                last_ctrl = None;
            }
            'H' => {
                let x = num(&mut i);
                p = pos2(if rel { p.x + x } else { x }, p.y);
                cur.push(p);
                last_ctrl = None;
            }
            'V' => {
                let y = num(&mut i);
                p = pos2(p.x, if rel { p.y + y } else { y });
                cur.push(p);
                last_ctrl = None;
            }
            'C' | 'S' => {
                let c1 = if cmd.eq_ignore_ascii_case(&'C') {
                    let x = num(&mut i);
                    let y = num(&mut i);
                    base + vec2(x, y)
                } else {
                    last_ctrl.map(|c| p + (p - c)).unwrap_or(p)
                };
                let x2 = num(&mut i);
                let y2 = num(&mut i);
                let x = num(&mut i);
                let y = num(&mut i);
                let c2 = base + vec2(x2, y2);
                let e = base + vec2(x, y);
                for k in 1..=12 {
                    let t = k as f32 / 12.0;
                    let u = 1.0 - t;
                    let q = p.to_vec2() * (u * u * u)
                        + c1.to_vec2() * (3.0 * u * u * t)
                        + c2.to_vec2() * (3.0 * u * t * t)
                        + e.to_vec2() * (t * t * t);
                    cur.push(q.to_pos2());
                }
                last_ctrl = Some(c2);
                p = e;
            }
            'Q' => {
                let cx = num(&mut i);
                let cy = num(&mut i);
                let x = num(&mut i);
                let y = num(&mut i);
                let c = base + vec2(cx, cy);
                let e = base + vec2(x, y);
                for k in 1..=10 {
                    let t = k as f32 / 10.0;
                    let u = 1.0 - t;
                    let q =
                        p.to_vec2() * (u * u) + c.to_vec2() * (2.0 * u * t) + e.to_vec2() * (t * t);
                    cur.push(q.to_pos2());
                }
                last_ctrl = None;
                p = e;
            }
            'A' => {
                let rx = num(&mut i);
                let ry = num(&mut i);
                let rot = num(&mut i);
                let large = num(&mut i) != 0.0;
                let sweep = num(&mut i) != 0.0;
                let x = num(&mut i);
                let y = num(&mut i);
                let e = base + vec2(x, y);
                arc(&mut cur, p, e, rx, ry, rot, large, sweep);
                p = e;
                last_ctrl = None;
            }
            _ => {
                i += 1;
            }
        }
    }
    flush(&mut subs, &mut cur, false);
    subs
}

/// SVG endpoint arc to points (spec F.6.5), appended without the start.
#[allow(clippy::too_many_arguments)]
fn arc(
    out: &mut Vec<Pos2>,
    p0: Pos2,
    p1: Pos2,
    rx: f32,
    ry: f32,
    rot_deg: f32,
    large: bool,
    sweep: bool,
) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-6 || ry < 1e-6 {
        out.push(p1);
        return;
    }
    let phi = rot_deg.to_radians();
    let (s, c) = phi.sin_cos();
    let dx = (p0.x - p1.x) / 2.0;
    let dy = (p0.y - p1.y) / 2.0;
    let x1 = c * dx + s * dy;
    let y1 = -s * dx + c * dy;
    let lam = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lam > 1.0 {
        rx *= lam.sqrt();
        ry *= lam.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut co = if den > 0.0 {
        (num / den).max(0.0).sqrt()
    } else {
        0.0
    };
    if large == sweep {
        co = -co;
    }
    let cxp = co * rx * y1 / ry;
    let cyp = -co * ry * x1 / rx;
    let cx = c * cxp - s * cyp + (p0.x + p1.x) / 2.0;
    let cy = s * cxp + c * cyp + (p0.y + p1.y) / 2.0;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let t1 = ang(1.0, 0.0, (x1 - cxp) / rx, (y1 - cyp) / ry);
    let mut dt = ang(
        (x1 - cxp) / rx,
        (y1 - cyp) / ry,
        (-x1 - cxp) / rx,
        (-y1 - cyp) / ry,
    );
    if !sweep && dt > 0.0 {
        dt -= std::f32::consts::TAU;
    } else if sweep && dt < 0.0 {
        dt += std::f32::consts::TAU;
    }
    let n = ((dt.abs() / (std::f32::consts::PI / 16.0)).ceil() as usize).max(2);
    for k in 1..=n {
        let t = t1 + dt * k as f32 / n as f32;
        let (st, ct) = t.sin_cos();
        out.push(pos2(
            cx + c * rx * ct - s * ry * st,
            cy + s * rx * ct + c * ry * st,
        ));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok {
    Cmd(char),
    Num(f32),
}

fn tokenize(d: &str) -> Vec<Tok> {
    let b = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let ch = b[i] as char;
        if ch.is_ascii_alphabetic() && ch != 'e' && ch != 'E' {
            out.push(Tok::Cmd(ch));
            i += 1;
        } else if ch == '-' || ch == '.' || ch.is_ascii_digit() {
            let st = i;
            let mut seen_dot = false;
            let mut seen_e = false;
            if b[i] == b'-' {
                i += 1;
            }
            while i < b.len() {
                let c = b[i] as char;
                if c.is_ascii_digit() {
                    i += 1;
                } else if c == '.' && !seen_dot && !seen_e {
                    seen_dot = true;
                    i += 1;
                } else if (c == 'e' || c == 'E') && !seen_e {
                    seen_e = true;
                    i += 1;
                    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            out.push(Tok::Num(d[st..i].parse().unwrap_or(0.0)));
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_keys_show_at_1x() {
        // the board's 1x keyboard: each key a quarter-strength pixel in its
        // row, two where the key sits on a pixel edge
        let ctx = eframe::egui::Context::default();
        let out = ctx.run(Default::default(), |ctx| {
            let p = ctx.layer_painter(eframe::egui::LayerId::background());
            // the Shortcuts board's rail icon, box at (22, 145)
            paint(&p, Icon::Keys, pos2(30.0, 153.0), 16.0, Color32::WHITE);
        });
        let mut keys = Vec::new();
        for s in &out.shapes {
            if let Shape::Mesh(m) = &s.shape {
                keys.extend(m.vertices.chunks(4).map(|q| (q[0].pos, q[0].color)));
            }
        }
        let quarter = Color32::WHITE.gamma_multiply(0.25);
        assert!(
            keys.iter().all(|&(p, c)| c == quarter && p.y == 151.0),
            "{keys:?}"
        );
        let xs: Vec<f32> = keys.iter().map(|(p, _)| p.x).collect();
        assert_eq!(xs, [25.0, 26.0, 28.0, 31.0, 33.0, 34.0]);
    }

    #[test]
    fn a_lone_dot_is_a_pixel_at_1x() {
        // Settings' info note, box at (229, 604): the "i" dot fills half of
        // the two pixels it straddles, in row 609
        let ctx = eframe::egui::Context::default();
        let out = ctx.run(Default::default(), |ctx| {
            let p = ctx.layer_painter(eframe::egui::LayerId::background());
            paint(&p, Icon::Info, pos2(237.0, 612.0), 16.0, Color32::WHITE);
        });
        let dots: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                Shape::Mesh(m) => Some(m.vertices.chunks(4).map(|q| (q[0].pos, q[0].color))),
                _ => None,
            })
            .flatten()
            .collect();
        let half = Color32::WHITE.gamma_multiply(0.5);
        assert_eq!(
            dots,
            [(pos2(236.0, 609.0), half), (pos2(237.0, 609.0), half)]
        );
    }

    #[test]
    fn compact_numbers_and_flags_tokenize() {
        // "a1.5 1.5 0 0 1 3 0" and ".6 2.9" style numbers
        let t = tokenize("M19 10v1a7 7 0 0 1-.6 2.9");
        assert_eq!(
            t,
            vec![
                Tok::Cmd('M'),
                Tok::Num(19.0),
                Tok::Num(10.0),
                Tok::Cmd('v'),
                Tok::Num(1.0),
                Tok::Cmd('a'),
                Tok::Num(7.0),
                Tok::Num(7.0),
                Tok::Num(0.0),
                Tok::Num(0.0),
                Tok::Num(1.0),
                Tok::Num(-0.6),
                Tok::Num(2.9)
            ]
        );
    }

    #[test]
    fn every_icon_flattens_inside_its_grid() {
        use Icon::*;
        for icon in [
            Live, Check, Keys, Settings, Mic, MicOff, Chevron, More, App, Warn, Info, Plug, Power,
            Quit, Replay, Play, Pause, Copy, External, Tick, Close, Minimize, Maximize, Stop, Hold,
            Speaker, MicGlyph, AppGlyph,
        ] {
            let subs = subpaths(icon);
            assert!(!subs.is_empty(), "{icon:?}");
            for s in subs.iter() {
                for p in &s.pts {
                    assert!(
                        p.x >= -0.5 && p.x <= 24.5 && p.y >= -0.5 && p.y <= 24.5,
                        "{icon:?} {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn arcs_land_on_their_endpoint() {
        // the mic's lower bow: from (19, 11) round to (5, 11) through (12, 18)
        let subs = parse_path("M19 10v1a7 7 0 0 1-14 0v-1");
        let pts = &subs[0].pts;
        assert!(pts
            .iter()
            .any(|p| (p.y - 18.0).abs() < 0.2 && (p.x - 12.0).abs() < 0.5));
        let end = pts[pts.len() - 1];
        assert!((end.x - 5.0).abs() < 1e-3 && (end.y - 10.0).abs() < 1e-3);
    }
}
