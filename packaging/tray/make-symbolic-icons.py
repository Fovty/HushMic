#!/usr/bin/env python3
"""Draw the symbolic (monochrome, theme coloured) tray icons.

Writes hushmic-mono[-state]-symbolic.svg into the hicolor status directories
next to this script. The canvas filling 16 px glyph goes to 16x16 (GNOME's
panel size), the glyph with Breeze's margins to 22x22, 24x24 and scalable, so
every other size and scale keeps Breeze's proportions. hicolor's fixed
directories only match at their own scale, hence the @2 copies.
The icons are plain filled paths, because GTK fills
every path when it recolours a symbolic icon (strokes would turn into blobs).
KDE recolours through the current-color-scheme stylesheet, GTK through the
-symbolic name; the error badge carries both desktops' class for red.

Needs: python3 with shapely (pip install shapely).
"""
import pathlib

from shapely.affinity import translate
from shapely.geometry import LineString, Point, Polygon, box
from shapely.ops import unary_union

ROOT = pathlib.Path(__file__).parent / "hicolor"
TEXT, NEGATIVE, HIGHLIGHT = "#232629", "#da4453", "#3daee9"


def vbar(x0, y0, y1):
    return box(x0, y0, x0 + 1, y1)


def ring_bottom(cx, cy, r_out, r_in):
    ring = Point(cx, cy).buffer(r_out, quad_segs=16).difference(Point(cx, cy).buffer(r_in, quad_segs=16))
    return ring.intersection(box(cx - r_out, cy, cx + r_out, cy + r_out))


def capsule(x0, y0, x1, y1):
    r = (x1 - x0) / 2
    cx = (x0 + x1) / 2
    return unary_union([box(x0, y0 + r, x1, y1 - r), Point(cx, y0 + r).buffer(r, quad_segs=12),
                        Point(cx, y1 - r).buffer(r, quad_segs=12)])


# Two glyphs, every edge on a whole pixel of its own grid. Breeze draws its
# panel icons 16 px tall on a 22 px canvas (24 px is the same with one more
# pixel of margin), and lets wide glyphs such as mic-on reach 18 px across;
# the 22 px glyph follows that so the icon matches its neighbours in a
# Plasma panel. The 16 px glyph fills its canvas the way GNOME's symbolic
# icons do and serves every other size.
GLYPH16 = dict(
    capsule=(6, 1, 10, 10), holder=(8, 8, 4, 3), legs_y=7,
    stand=[box(7, 11.5, 9, 14), box(5, 14, 11, 15)],
    noise_in=[vbar(0, 5, 8), vbar(2, 3, 10)],
    noise_out=[vbar(13, 3, 10), vbar(15, 5, 8)],
    clean_out=box(13, 6, 16, 7),
    slash=((2.4, 1.4), (13.6, 15.0), 0.6, 1.5),
    badge=([(12.25, 9.6), (15.4, 15.1), (9.1, 15.1)], 0.55, 0.85),
    bang=[box(11.8, 10.8, 12.7, 13.25), Point(12.25, 14.25).buffer(0.5, quad_segs=8)],
)
GLYPH22 = dict(
    capsule=(8, 3, 14, 14), holder=(11, 11, 5, 4), legs_y=9,
    stand=[box(10, 15.5, 12, 18), box(7, 18, 15, 19)],
    noise_in=[vbar(2, 9, 12), vbar(4, 6, 15)],
    noise_out=[vbar(17, 6, 15), vbar(19, 9, 12)],
    clean_out=box(17, 10, 20, 11),
    slash=((3.6, 3.4), (18.4, 18.8), 0.65, 1.7),
    badge=([(16.2, 12.6), (20.0, 19.2), (12.4, 19.2)], 0.6, 1.0),
    bang=[box(15.65, 14.0, 16.75, 17.0), Point(16.2, 18.35).buffer(0.6, quad_segs=8)],
)
# canvas size -> (theme directory, glyph, margin added around the glyph)
CANVASES = [("16x16", 16, GLYPH16, 0), ("16x16@2", 16, GLYPH16, 0),
            ("22x22", 22, GLYPH22, 0), ("22x22@2", 22, GLYPH22, 0),
            ("24x24", 24, GLYPH22, 1), ("24x24@2", 24, GLYPH22, 1),
            ("scalable", 22, GLYPH22, 0)]


def shapes(g):
    cap = capsule(*g["capsule"])
    cx, cy, r_out, r_in = g["holder"]
    holder = unary_union([ring_bottom(cx, cy, r_out, r_in),
                          box(cx - r_out, g["legs_y"], cx - r_in, cy),
                          box(cx + r_in, g["legs_y"], cx + r_out, cy)])
    stand = unary_union(g["stand"])
    mic = unary_union([cap, holder, stand])
    hollow = unary_union([cap.difference(cap.buffer(-1, quad_segs=12)), holder, stand])
    a, b, w, gap = g["slash"]
    line = LineString([a, b])
    tri, grow, clear = g["badge"]
    badge = Polygon(tri).buffer(grow, quad_segs=8)
    return dict(
        mic=mic, hollow=hollow, core=cap.buffer(-1, quad_segs=12),
        noise_in=unary_union(g["noise_in"]), noise_out=unary_union(g["noise_out"]),
        clean_out=g["clean_out"],
        slash=line.buffer(w, quad_segs=6), slash_gap=line.buffer(gap, cap_style="flat"),
        badge=badge.difference(unary_union(g["bang"])), badge_gap=badge.buffer(clear, quad_segs=8),
    )


def path_d(geom):
    polys = getattr(geom, "geoms", [geom])
    out = []
    for p in polys:
        for ring in [p.exterior, *p.interiors]:
            pts = list(ring.simplify(0.01).coords)[:-1]
            out.append("M" + "L".join(f"{x:.2f} {y:.2f}".replace(".00", "") for x, y in pts) + "Z")
    return "".join(out)


def svg(size, margin, parts):
    body = "\n".join(
        f'  <path class="{cls}" fill="currentColor" fill-rule="evenodd"{extra} '
        f'd="{path_d(translate(g, margin, margin))}"/>'
        for g, cls, extra in parts)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
  <defs>
    <style id="current-color-scheme" type="text/css">
      .ColorScheme-Text {{ color:{TEXT}; }}
      .ColorScheme-NegativeText {{ color:{NEGATIVE}; }}
      .ColorScheme-Highlight {{ color:{HIGHLIGHT}; }}
    </style>
  </defs>
{body}
</svg>
'''


T, N = "ColorScheme-Text", "ColorScheme-NegativeText error"
# KDE only: the accent colour. GTK has no such class and fills it as foreground.
H = "ColorScheme-Highlight"


def icons(s):
    return {
        # suppressing: noise in, clean out
        "hushmic-mono": [(unary_union([s["hollow"], s["noise_in"], s["clean_out"]]), T, ""), (s["core"], H, "")],
        # off: the virtual microphone is gone, a faint mic and nothing moving
        "hushmic-mono-off": [(s["mic"], T, ' opacity="0.35"')],
        # bypass: the mic is live but hollow, the noise goes out as it came in
        "hushmic-mono-bypass": [(unary_union([s["hollow"], s["noise_in"], s["noise_out"]]), T, "")],
        # mute: the usual struck-through microphone
        "hushmic-mono-mute": [(s["mic"].difference(s["slash_gap"]), T, ""), (s["slash"], N, "")],
        # error: mic with a warning badge in the desktop's error colour
        "hushmic-mono-error": [(s["mic"].difference(s["badge_gap"]), T, ""), (s["badge"], N, "")],
    }


if __name__ == "__main__":
    for directory, size, glyph, margin in CANVASES:
        out = ROOT / directory / "status"
        out.mkdir(parents=True, exist_ok=True)
        for name, parts in icons(shapes(glyph)).items():
            (out / f"{name}-symbolic.svg").write_text(svg(size, margin, parts))
        print("wrote", out)
