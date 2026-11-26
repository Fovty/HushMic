#!/usr/bin/env python3
"""Board vs egui parity shots for the HushMic window.

For each case in scenes.json (or the ones named on the command line), and
each theme / size / scale asked for, this renders
  - the board state with the board renderer (board-shot.mjs, headless Chrome),
  - the egui scene with the screenshot harness (the `ui-shot` example, Xvfb),
and writes to the output folder (default ~/hm-v1/shots/parity):
  <case>-<theme>-<size>-<scale>x-board.png, -egui.png,
  -side.png (board | egui | diff), -diff.png (|board − egui| × 4),
plus one line of numbers per shot (mean difference, share of pixels that
differ by more than 24 of 255) and a summary.tsv.

usage: parity.py [CASE ...] [--themes dark,light] [--sizes default,minimum]
                 [--scales 1,2] [--out DIR] [--no-board] [--no-egui] [--list]

Needs: node + google-chrome (board), xvfb-run (egui), python3-pil + numpy.
The egui binary is built with `cargo build -p hushmic --example ui-shot`
(this script builds it unless --no-build). Run it from the repository.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, '..', '..'))
SIZES = {'default': (960, 640), 'minimum': (720, 520)}


def load_cases():
    with open(os.path.join(HERE, 'scenes.json')) as f:
        return json.load(f)['cases']


def run(cmd, env=None, cwd=None):
    r = subprocess.run(cmd, env=env, cwd=cwd, capture_output=True, text=True)
    if r.returncode != 0:
        sys.stderr.write(' '.join(cmd) + '\n' + r.stdout + r.stderr)
        raise SystemExit(1)
    return r.stdout


def board_shot(case, theme, size, scale, out):
    props = dict(case.get('props', {}))
    cmd = ['node', os.path.join(HERE, 'board-shot.mjs'), '--board', case['board'], '--props', json.dumps(props),
           '--theme', theme, '--size', size, '--dpr', str(scale), '--out', out, '--at', str(case.get('at', 3000))]
    if case.get('ticks') is not None:
        cmd += ['--ticks', str(case['ticks'])]
    for js in case.get('eval', []):
        cmd += ['--eval', js]
    run(cmd)


def egui_shots(jobs, scale, binary):
    """jobs: list of (scene, theme, (w, h), out, params). One process per scale."""
    if not jobs:
        return
    with tempfile.NamedTemporaryFile('w', suffix='.jobs', delete=False) as f:
        for scene, theme, (w, h), out, params in jobs:
            f.write(f'{scene} {theme} {w}x{h} {out} {params}\n')
        path = f.name
    env = dict(os.environ, WINIT_X11_SCALE_FACTOR=str(scale), HUSHMIC_UI_SOFTWARE='0', WINIT_UNIX_BACKEND='x11')
    env.pop('WAYLAND_DISPLAY', None)
    try:
        run(['nice', '-n', '10', 'xvfb-run', '-a', '-s', '-screen 0 2400x1600x24', binary, '--jobs', path], env=env)
    finally:
        os.unlink(path)


def label(img, text):
    d = ImageDraw.Draw(img)
    try:
        font = ImageFont.truetype('DejaVuSans.ttf', 14)
    except OSError:
        font = ImageFont.load_default()
    d.rectangle([0, 0, d.textlength(text, font=font) + 12, 22], fill=(255, 0, 128))
    d.text((6, 3), text, fill=(255, 255, 255), font=font)


def compare(board_png, egui_png, side_png, diff_png, region=None, scale=1):
    """region: [x, y, w, h] in CSS px, the part of the window the case covers."""
    a = Image.open(board_png).convert('RGB')
    b = Image.open(egui_png).convert('RGB')
    if a.size != b.size:
        b = b.resize(a.size)
    if region:
        x, y, w, h = [v * scale for v in region]
        a, b = a.crop((x, y, x + w, y + h)), b.crop((x, y, x + w, y + h))
    A = np.asarray(a).astype(np.int16)
    B = np.asarray(b).astype(np.int16)
    d = np.abs(A - B).max(axis=2)
    mean = float(d.mean())
    share = float((d > 24).mean() * 100)
    heat = np.clip(d * 4, 0, 255).astype(np.uint8)
    Image.fromarray(heat).save(diff_png)
    w, h = a.size
    side = Image.new('RGB', (w * 3, h), (255, 0, 128))
    side.paste(a, (0, 0))
    side.paste(b, (w, 0))
    side.paste(Image.fromarray(heat).convert('RGB'), (2 * w, 0))
    for i, t in enumerate(['board', 'egui', 'diff x4']):
        tile = side.crop((i * w, 0, (i + 1) * w, h))
        label(tile, t)
        side.paste(tile, (i * w, 0))
    side.save(side_png)
    return mean, share


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('cases', nargs='*')
    ap.add_argument('--themes', default='dark,light')
    ap.add_argument('--sizes', default='default,minimum')
    ap.add_argument('--scales', default='1,2')
    ap.add_argument('--out', default=os.path.join(os.path.expanduser('~'), 'hm-v1', 'shots', 'parity'))
    ap.add_argument('--no-board', action='store_true', help='reuse the board shots already in --out')
    ap.add_argument('--no-egui', action='store_true', help='reuse the egui shots already in --out')
    ap.add_argument('--no-build', action='store_true')
    ap.add_argument('--list', action='store_true')
    a = ap.parse_args()
    cases = load_cases()
    if a.list:
        for c in cases:
            print(f"{c['case']:<28} {c['board']:<16} {c['scene']} {c.get('params', '')}")
        return
    if a.cases:
        cases = [c for c in cases if c['case'] in a.cases]
        if not cases:
            raise SystemExit('no such case')
    os.makedirs(a.out, exist_ok=True)
    target = os.environ.get('CARGO_TARGET_DIR', os.path.join(ROOT, 'target'))
    binary = os.path.join(target, 'debug', 'examples', 'ui-shot')
    if not a.no_egui and not a.no_build:
        run(['nice', '-n', '10', 'cargo', 'build', '-j', '4', '-q', '-p', 'hushmic', '--example', 'ui-shot'], cwd=ROOT)
    themes, sizes, scales = a.themes.split(','), a.sizes.split(','), [int(s) for s in a.scales.split(',')]
    shots = []
    for c in cases:
        for theme in themes:
            for size in sizes:
                if size not in c.get('sizes', ['default', 'minimum']):
                    continue
                for scale in scales:
                    stem = os.path.join(a.out, f"{c['case']}-{theme}-{size}-{scale}x")
                    shots.append((c, theme, size, scale, stem))
    if not a.no_board:
        for c, theme, size, scale, stem in shots:
            board_shot(c, theme, size, scale, stem + '-board.png')
    if not a.no_egui:
        for scale in scales:
            jobs = [(c['scene'], theme, SIZES[size], stem + '-egui.png', c.get('params', ''))
                    for c, theme, size, s, stem in shots if s == scale]
            egui_shots(jobs, scale, binary)
    rows = []
    for c, theme, size, scale, stem in shots:
        mean, share = compare(stem + '-board.png', stem + '-egui.png', stem + '-side.png', stem + '-diff.png',
                              c.get('region'), scale)
        rows.append((os.path.basename(stem), mean, share))
        print(f'{os.path.basename(stem):<44} mean {mean:5.2f}  differing {share:5.2f} %  {stem}-side.png')
    with open(os.path.join(a.out, 'summary.tsv'), 'a') as f:
        for r in rows:
            f.write('%s\t%.3f\t%.3f\n' % r)


if __name__ == '__main__':
    main()
