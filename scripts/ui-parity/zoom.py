#!/usr/bin/env python3
"""Zoom into one region of a parity shot: board above, egui below, diff at
the bottom, scaled up with nearest-neighbour pixels; prints the (dx, dy)
shift of the egui region that best matches the board (how far it is off).

usage: zoom.py STEM X,Y,W,H [--k 4] [--out FILE]
  STEM: a parity shot without the suffix, e.g.
        ~/hm-v1/shots/parity/main-on-dark-default-1x
  X,Y,W,H: the region in that shot's pixels.
"""
import argparse

import numpy as np
from PIL import Image


def best_shift(a, b, r=4):
    best = (1e9, 0, 0)
    h, w = a.shape[:2]
    for dy in range(-r, r + 1):
        for dx in range(-r, r + 1):
            ya, yb = max(0, dy), max(0, -dy)
            xa, xb = max(0, dx), max(0, -dx)
            hh, ww = h - abs(dy), w - abs(dx)
            if hh <= 0 or ww <= 0:
                continue
            d = np.abs(a[ya:ya + hh, xa:xa + ww] - b[yb:yb + hh, xb:xb + ww]).mean()
            if d < best[0]:
                best = (d, dx, dy)
    return best


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('stem')
    ap.add_argument('region')
    ap.add_argument('--k', type=int, default=4)
    ap.add_argument('--out')
    a = ap.parse_args()
    x, y, w, h = [int(v) for v in a.region.split(',')]
    A = Image.open(a.stem + '-board.png').convert('RGB').crop((x, y, x + w, y + h))
    B = Image.open(a.stem + '-egui.png').convert('RGB').crop((x, y, x + w, y + h))
    na, nb = np.asarray(A).astype(np.float32), np.asarray(B).astype(np.float32)
    d0 = np.abs(na - nb).mean()
    d, dx, dy = best_shift(na, nb)
    print(f'region {a.region}: mean diff {d0:.2f}; best match moving egui by dx={dx} dy={dy} (diff {d:.2f})')
    diff = Image.fromarray(np.clip(np.abs(na - nb).max(axis=2) * 4, 0, 255).astype(np.uint8)).convert('RGB')
    out = Image.new('RGB', (w * a.k, (h * 3 + 2) * a.k), (255, 0, 128))
    for i, im in enumerate([A, B, diff]):
        out.paste(im.resize((w * a.k, h * a.k), Image.NEAREST), (0, i * (h + 1) * a.k))
    path = a.out or f'{a.stem}-zoom-{x}-{y}.png'
    out.save(path)
    print(path)


if __name__ == '__main__':
    main()
