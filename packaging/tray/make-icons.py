#!/usr/bin/env python3
"""Draw every HushMic icon from the F1 "Brighten" mark.

The mark: grey noise bars that brighten towards one cyan line lying down
after them. It comes in three cuts, each pixel-exact on its own grid:

  display  24-unit grid, four bars and the line
  small    16-unit grid, three bars and the line (tray glyphs, small icons)
  micro    16-unit grid, two bars and the line (the 16 px app icon only)

Writes the hicolor tree next to this script, replacing it whole once every
file is drawn:

  <size>/apps/hushmic.png, scalable/apps/hushmic.svg
      The app icon: the mark on a dark tile. Its 256 px PNG is also copied
      to packaging/hushmic-256.png, the window icon the binary embeds.
  <size>/status/hushmic-tray[-state].png
      The colour tray set, for desktops that do not recolour icons. A PNG
      cannot follow the panel's theme, so it is drawn in mid-tones that
      hold on dark and on light panels.
  <dir>/status/hushmic-mono[-state]-symbolic.svg
      The monochrome tray set KDE and GNOME recolour to the panel's text
      colour; the mute slash and the attention badge take the desktop's
      negative colour (KDE's ColorScheme-NegativeText, GTK's error class).

Tray states: active, off (the bars lie down as dots, the line grey: a shape
of its own, drawn at full strength so it holds on a light panel), bypass
(standing bars, no line, and an open ring cut out of the corner: noise
passes), mute (a slash cut out of the mark), error (a warning badge cut out
of the mark; also shown for an unplugged or silent microphone). Both tray sets put the 16 px glyph on Breeze's 22 and 24 px
canvases with Breeze's margins, so the icon matches its neighbours in a
Plasma panel and does not change size when the set changes.

Every shape is a filled path: GTK fills every path when it recolours a
symbolic icon (a stroke would turn into a blob), and Qt's SVG renderer has
no masks, so the cut-outs are computed here.

Needs: python3 with shapely (pip install shapely), and resvg
(https://github.com/linebender/resvg) on PATH for the PNGs. The committed
files came from shapely 2.2.0 and resvg 0.45.1; other versions draw the
same icons but may not match them byte for byte.
"""
import pathlib
import shutil
import subprocess
import tempfile

from shapely.affinity import scale, translate
from shapely.geometry import LineString, Point, Polygon, box
from shapely.ops import unary_union

ROOT = pathlib.Path(__file__).parent / "hicolor"
WINDOW_ICON = pathlib.Path(__file__).parent.parent / "hushmic-256.png"

# The design system's dark theme: the app icon's tile is always dark.
INK, ACCENT = "#fafafa", "#00f0f8"
# The colour tray set: mid-tones that read on dark and on light panels.
TRAY_INK, TRAY_ACCENT, TRAY_DANGER = "#9c9c9c", "#00b2ba", "#f0524a"
# Breeze's colours: the fallback KDE's stylesheet replaces.
TEXT, NEGATIVE = "#232629", "#da4453"

# The cuts. Bars are fully rounded, 2 units wide unless `width` says
# otherwise: (x, y, height, opacity). The opacities are F1's: the bars
# brighten towards the line. `glow`: the blur of the app icon's line glow.
DISPLAY = dict(
    grid=24, glow=0.9,
    noise=[(2, 5, 14, .40), (5, 2, 20, .55), (8, 7, 10, .72), (11, 4.5, 15, .90)],
    line=(14, 11, 8),
    # bypass: three more bars stand where the line lies (the settle's first frame)
    bypass=[(14, 3, 18, .90), (17, 6, 12, .90), (20, 3.5, 17, .90)],
    slash=((2.8, 2.2), (21.8, 21.6), 2.0), slash_gap=((2, 1.4), (22.6, 22.4), 4.4),
    badge=([(18, 13.6), (22.8, 22), (13.2, 22)], 1.4, 4.4),
    bang=((17.35, 16.2, 1.3, 3.2), (18, 20.6, 0.75)),
    # bypass's ring: centre, outer and inner radius, the gap cut around it
    ring=((19, 18.4), 3.6, 2.0, 1.4),
)
SMALL = dict(
    grid=16,
    noise=[(0, 4, 8, .45), (3, 1, 14, .65), (6, 5, 6, .88)],
    line=(9, 7, 7),
    bypass=[(1, 4, 8, 1), (4, 1, 14, 1), (7, 5, 6, 1), (10, 3, 10, 1), (13, 5, 6, 1)],
    slash=((1.6, 1.1), (14.9, 14.4), 1.5), slash_gap=((1, 0.5), (15.5, 15), 3.2),
    badge=([(12, 9.6), (15.4, 15.4), (8.6, 15.4)], 1.1, 3.4),
    bang=((11.55, 11.1, 0.9, 2.5), (12, 14.45, 0.55)),
    ring=((12.6, 12.6), 3.0, 1.6, 1.1),
)
MICRO = dict(grid=16, noise=[(2, 4, 8, .55), (5, 2, 12, .85)], line=(8, 7, 6))
# The display cut hinted for the 64 px app icon, in px (1.5 px per unit):
# the bars snap to whole pixels (gaps of 1 and 2 px for 1.5) and the whole
# mark sits half a pixel low so the line fills whole rows.
# The display cut at 1 px per unit (the 32 and 48 px app icons): the fourth
# bar grows by one unit so both its ends land on whole pixels.
DISPLAY_1PX = dict(DISPLAY, noise=DISPLAY["noise"][:3] + [(11, 4, 16, .90)])
HINTED_64 = dict(
    grid=24, width=3, glow=1.35,
    noise=[(17, 22, 21, .40), (21, 17.5, 30, .55), (26, 25, 15, .72), (30, 21.25, 22.5, .90)],
    line=(35, 31, 12),
)
# In the tray the faintest bar keeps a little more (about 3:1 on a light panel).
TRAY_RAMP = [.50, .70, .90]
# Off: the bars lie down as dots and the line is no longer the voice, at
# full strength in the colour set (its mid-tones hold on a light panel),
# a little faded in the monochrome one (the panel's own text colour).
OFF, TRAY_OFF = 0.6, 1.0
STATES = ["", "-off", "-bypass", "-mute", "-error"]

# App icon, per size: tile inset, tile radius, cut, mark origin, unit (px).
APP_SIZES = {
    16: (0, 3.5, MICRO, 0, 1),
    22: (1, 4.5, SMALL, 3, 1),
    24: (1, 5, SMALL, 4, 1),
    32: (1, 7, DISPLAY_1PX, 4, 1),
    48: (2, 10, DISPLAY_1PX, 12, 1),
    64: (2, 14, HINTED_64, 0, 1),
    128: (4, 28, DISPLAY, 28, 3),
    256: (8, 56, DISPLAY, 56, 6),
    512: (16, 112, DISPLAY, 112, 12),
}
# Colour tray PNGs, per canvas: cut, unit, margin. 32 and 48 are the 16 and
# 24 px designs at twice the size; from 64 on the display cut takes over.
TRAY_SIZES = {
    16: (SMALL, 1, 0),
    22: (SMALL, 1, 3),
    24: (SMALL, 1, 4),
    32: (SMALL, 2, 0),
    48: (SMALL, 2, 8),
    64: (DISPLAY, 2, 8),
    128: (DISPLAY, 4, 16),
    256: (DISPLAY, 8, 32),
}
# Symbolic SVGs, per theme directory: canvas, margin. hicolor's fixed
# directories only match at their own scale, hence the @2 copies.
SYMBOLIC_DIRS = {
    "16x16": (16, 0), "16x16@2": (16, 0),
    "22x22": (22, 3), "22x22@2": (22, 3),
    "24x24": (24, 4), "24x24@2": (24, 4),
    "scalable": (22, 3),
}


# --- geometry -----------------------------------------------------------------

def num(v):
    s = f"{v:.3f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def rounded(x, y, w, h, r):
    return box(x + r, y + r, x + w - r, y + h - r).buffer(r, quad_segs=16)


def rounded_d(x, y, w, h, r):
    """The exact SVG path of a rounded rectangle."""
    f, a = num, f"A{num(r)} {num(r)} 0 0 1 "
    top = f"H{f(x + w - r)}" if w > 2 * r else ""
    right = f"V{f(y + h - r)}" if h > 2 * r else ""
    bottom = f"H{f(x + r)}" if w > 2 * r else ""
    left = f"V{f(y + r)}" if h > 2 * r else ""
    return (f"M{f(x + r)} {f(y)}{top}{a}{f(x + w)} {f(y + r)}{right}{a}{f(x + w - r)} {f(y + h)}"
            f"{bottom}{a}{f(x)} {f(y + h - r)}{left}{a}{f(x + r)} {f(y)}Z")


def polygon_d(geom):
    out = []
    for p in getattr(geom, "geoms", [geom]):
        for ring in [p.exterior, *p.interiors]:
            pts = list(ring.simplify(0.002).coords)[:-1]
            out.append("M" + "L".join(f"{num(x)} {num(y)}" for x, y in pts) + "Z")
    return "".join(out)


class Part:
    """One filled shape of the mark. Role: noise, voice or neg. A bar keeps
    its exact arcs until a cut-out changes it."""

    def __init__(self, geom, role, opacity=1.0, rect=None):
        self.geom, self.role, self.opacity, self.rect = geom, role, opacity, rect

    def cut(self, hole):
        if not self.geom.intersects(hole):
            return self
        return Part(self.geom.difference(hole), self.role, self.opacity)

    def d(self, origin=0.0, unit=1.0):
        if self.rect is not None:
            x, y, w, h = self.rect
            return rounded_d(origin + x * unit, origin + y * unit, w * unit, h * unit, min(w, h) * unit / 2)
        return polygon_d(translate(scale(self.geom, unit, unit, origin=(0, 0)), origin, origin))


def bar(x, y, w, h, role, opacity):
    return Part(rounded(x, y, w, h, min(w, h) / 2), role, opacity, rect=(x, y, w, h))


def mark(cut, state="", ramp=None, off=OFF):
    """The mark's parts in one tray state ("" is active)."""
    if state == "-bypass":
        bars = cut["bypass"] if cut is SMALL else cut["noise"] + cut["bypass"]
        parts = [bar(x, y, 2, h, "noise", o) for x, y, h, o in bars]
        (cx, cy), outer, inner, gap = cut["ring"]
        ring = Point(cx, cy).buffer(outer, quad_segs=16).difference(
            Point(cx, cy).buffer(inner, quad_segs=16))
        hole = Point(cx, cy).buffer(outer + gap, quad_segs=16)
        return [p.cut(hole) for p in parts] + [Part(ring, "noise")]
    ramp = ramp or [o for *_, o in cut["noise"]]
    w = cut.get("width", 2)
    parts = [bar(x, y, w, h, "noise", o) for (x, y, h, _), o in zip(cut["noise"], ramp)]
    lx, ly, lw = cut["line"]
    parts.append(bar(lx, ly, lw, w, "voice", 1.0))
    if state == "-off":
        # each bar lies down to a dot on its centre line; the line stays
        dots = []
        for x, y, h, _ in cut["noise"]:
            cy = y + h / 2
            dots.append(bar(x, cy - w / 2, w, w, "noise", off))
        line = parts[-1]
        return dots + [Part(line.geom, "noise", off, line.rect)]
    if state == "-mute":
        (a, b, w), (ga, gb, gw) = cut["slash"], cut["slash_gap"]
        gap = LineString([ga, gb]).buffer(gw / 2, cap_style="flat")
        slash = LineString([a, b]).buffer(w / 2, quad_segs=16)
        return [p.cut(gap) for p in parts] + [Part(slash, "neg")]
    if state == "-error":
        tri, stroke, gap = cut["badge"]
        (bx, by, bw, bh), (cx, cy, cr) = cut["bang"]
        bang = unary_union([rounded(bx, by, bw, bh, bw / 3), Point(cx, cy).buffer(cr, quad_segs=16)])
        badge = Polygon(tri).buffer(stroke / 2, quad_segs=16).difference(bang)
        hole = Polygon(tri).buffer(gap / 2, quad_segs=16)
        return [p.cut(hole) for p in parts] + [Part(badge, "neg")]
    return parts


# --- SVG ------------------------------------------------------------------------

def svg(size, body, defs=""):
    defs = f"  <defs>\n{defs}  </defs>\n" if defs else ""
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{num(size)}" height="{num(size)}" '
            f'viewBox="0 0 {num(size)} {num(size)}">\n{defs}{body}</svg>\n')


def opacity(o):
    return "" if o >= 0.9995 else f' opacity="{num(round(o, 4))}"'


def painted(parts, origin, unit, colours):
    return "".join(f'  <path fill="{colours[p.role]}"{opacity(p.opacity)} d="{p.d(origin, unit)}"/>\n'
                   for p in parts)


def symbolic_svg(state, canvas, margin):
    style = ('    <style id="current-color-scheme" type="text/css">\n'
             f"      .ColorScheme-Text {{ color:{TEXT}; }}\n"
             f"      .ColorScheme-NegativeText {{ color:{NEGATIVE}; }}\n"
             "    </style>\n")
    body = ""
    for p in mark(SMALL, state, TRAY_RAMP):
        cls = "ColorScheme-NegativeText error" if p.role == "neg" else "ColorScheme-Text"
        body += f'  <path class="{cls}" fill="currentColor"{opacity(p.opacity)} d="{p.d(margin)}"/>\n'
    return svg(canvas, body, style)


def tray_svg(state, size):
    cut, unit, margin = TRAY_SIZES[size]
    colours = {"noise": TRAY_INK, "voice": TRAY_ACCENT, "neg": TRAY_DANGER}
    parts = mark(cut, state, TRAY_RAMP if cut is SMALL else None, TRAY_OFF)
    return svg(size, painted(parts, margin, unit, colours))


def app_svg(size):
    """The app icon: a dark tile lit from the top, the mark at 56 %, and the
    only glow of the system, on the line."""
    inset, radius, cut, origin, unit = APP_SIZES[size]
    tile = size - 2 * inset
    edge = size / 256
    defs = ('    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">\n'
            '      <stop offset="0" stop-color="#1c1c1c"/>\n'
            '      <stop offset="1" stop-color="#050505"/>\n'
            '    </linearGradient>\n'
            '    <linearGradient id="edge" x1="0" y1="0" x2="0" y2="1">\n'
            '      <stop offset="0" stop-color="#ffffff" stop-opacity="0.26"/>\n'
            '      <stop offset="0.3" stop-color="#ffffff" stop-opacity="0.08"/>\n'
            '      <stop offset="1" stop-color="#ffffff" stop-opacity="0.05"/>\n'
            '    </linearGradient>\n')
    body = f'  <path fill="url(#tile)" d="{rounded_d(inset, inset, tile, tile, radius)}"/>\n'
    if size >= 32:
        body += (f'  <path fill="none" stroke="url(#edge)" stroke-width="{num(edge)}" '
                 f'd="{rounded_d(inset + edge / 2, inset + edge / 2, tile - edge, tile - edge, radius - edge / 2)}"/>\n')
    parts = mark(cut)
    if "glow" in cut:
        defs += ('    <filter id="glow" x="-50%" y="-300%" width="200%" height="700%">\n'
                 f'      <feGaussianBlur stdDeviation="{num(cut["glow"] * unit)}"/>\n'
                 '    </filter>\n')
        line = [p for p in parts if p.role == "voice"][0]
        body += f'  <path fill="{ACCENT}" opacity="0.5" filter="url(#glow)" d="{line.d(origin, unit)}"/>\n'
    body += painted(parts, origin, unit, {"noise": INK, "voice": ACCENT})
    return svg(size, body, defs)


# --- output -------------------------------------------------------------------------

def render(svg_text, png, tmp):
    src = tmp / (png.stem + ".svg")
    src.write_text(svg_text)
    png.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["resvg", str(src), str(png)], check=True)


def main():
    if not shutil.which("resvg"):
        raise SystemExit("resvg not found on PATH (https://github.com/linebender/resvg)")
    # draw into a fresh tree beside the old one and swap at the end, so a
    # failure halfway leaves the committed tree as it was
    with tempfile.TemporaryDirectory(dir=ROOT.parent) as tmp:
        tmp = pathlib.Path(tmp)
        tree = tmp / "hicolor"
        for size in APP_SIZES:
            render(app_svg(size), tree / f"{size}x{size}" / "apps" / "hushmic.png", tmp)
        (tree / "scalable" / "apps").mkdir(parents=True)
        (tree / "scalable" / "apps" / "hushmic.svg").write_text(app_svg(256))
        for size in TRAY_SIZES:
            for state in STATES:
                render(tray_svg(state, size), tree / f"{size}x{size}" / "status" / f"hushmic-tray{state}.png", tmp)
        for directory, (canvas, margin) in SYMBOLIC_DIRS.items():
            out = tree / directory / "status"
            out.mkdir(parents=True, exist_ok=True)
            for state in STATES:
                (out / f"hushmic-mono{state}-symbolic.svg").write_text(symbolic_svg(state, canvas, margin))
        if ROOT.exists():
            shutil.rmtree(ROOT)
        tree.rename(ROOT)
    shutil.copyfile(ROOT / "256x256" / "apps" / "hushmic.png", WINDOW_ICON)
    print("wrote", ROOT)


if __name__ == "__main__":
    main()
