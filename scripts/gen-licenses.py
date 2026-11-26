#!/usr/bin/env python3
"""crates/hushmic/src/ui/licenses_crates.rs: the Rust crates HushMic ships
and their licence texts, for About › Licenses.

The crates are what `cargo tree -e normal` says the app and the plugin link
on Linux (no build or dev dependencies). Each crate's own licence files are
used (LICENSE*, COPYING*, NOTICE*, UNLICENSE, and the fonts' licences in a
subfolder); a crate that ships none gets the standard text of its licence
from crates/hushmic/licenses/spdx/ (MIT with the crate's authors as the
copyright holders). Equal texts are kept once.

Run it after Cargo.lock changes; `--check` fails when the file is stale
(tests and CI).

usage: gen-licenses.py [--check]
"""
import json, os, re, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, 'crates/hushmic/src/ui/licenses_crates.rs')
SPDX = os.path.join(ROOT, 'crates/hushmic/licenses/spdx')
TARGET = 'x86_64-unknown-linux-gnu'
# what every package builds with (Opus recordings through libopus)
FEATURES = 'hushmic/opus'
# the standard texts a crate with no licence file of its own gets, in the
# order they are preferred within an `OR`
FALLBACK = ['MIT', 'Apache-2.0', 'MPL-2.0', 'CC0-1.0']
LICENSE_FILE = re.compile(r'(?i)^(un)?(licen[cs]e|copying|notice)')
FONT_LICENSE = re.compile(r'(?i)(licen[cs]e|^ofl|^ufl)')


def cargo(*args):
    return subprocess.run(['cargo', *args], cwd=ROOT, check=True,
                          capture_output=True, text=True).stdout


def shipped():
    """(name, version) of every crate the app and the plugin link."""
    out = cargo('tree', '--locked', '-e', 'normal', '--target', TARGET,
                '-p', 'hushmic', '-p', 'dpdfnet-ladspa', '--features', FEATURES,
                '--prefix', 'none', '-f', '{p}')
    crates = set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) < 2 or not parts[1].startswith('v'):
            continue
        if len(parts) > 2 and parts[2].startswith('('):
            tag = parts[2]
            # a path crate: HushMic's own, unless it is a vendored one
            # (someone else's crate, patched: vendor/*/README.hushmic.md)
            if not tag.startswith(('(*', '(proc-macro')) and '/vendor/' not in tag:
                continue
        crates.add((parts[0], parts[1][1:]))
    return crates


def tidy(text):
    # tabs as spaces: the sheet wraps the texts by their characters
    text = text.replace('\r\n', '\n').replace('\r', '\n')
    lines = [l.expandtabs(8).rstrip() for l in text.split('\n')]
    while lines and not lines[0]:
        lines.pop(0)
    while lines and not lines[-1]:
        lines.pop()
    return '\n'.join(lines)


def own_texts(pkg):
    d = os.path.dirname(pkg['manifest_path'])
    found = []
    for f in sorted(os.listdir(d)):
        p = os.path.join(d, f)
        if os.path.isfile(p) and LICENSE_FILE.match(f):
            found.append(p)
    # fonts and other bundled data keep theirs in a folder
    for sub in sorted(os.listdir(d)):
        sp = os.path.join(d, sub)
        if not os.path.isdir(sp) or sub in ('src', 'tests', 'benches', 'examples', 'target'):
            continue
        for f in sorted(os.listdir(sp)):
            p = os.path.join(sp, f)
            text_like = f.endswith(('.txt', '.md')) or '.' not in f
            if os.path.isfile(p) and FONT_LICENSE.search(f) and text_like:
                found.append(p)
    texts = []
    for p in found:
        with open(p, encoding='utf-8', errors='replace') as fh:
            t = tidy(fh.read())
        if t:
            texts.append(t)
    return texts


def standard_text(pkg):
    expr = pkg.get('license') or ''
    ids = re.findall(r'[A-Za-z0-9.+-]+', expr.replace('/', ' OR '))
    for want in FALLBACK:
        if want in ids:
            with open(os.path.join(SPDX, want + '.txt'), encoding='utf-8') as fh:
                t = tidy(fh.read())
            if want == 'MIT':
                authors = [re.sub(r'\s*<[^>]*>', '', a) for a in pkg.get('authors') or []]
                holder = ', '.join(a for a in authors if a) or f'the {pkg["name"]} authors'
                t = t.replace('{copyright}', holder)
            return t
    sys.exit(f'{pkg["name"]} {pkg["version"]}: no licence file and no standard text for {expr!r}')


def rust_str(t):
    hashes = '#'
    while f'"{hashes}' in t:
        hashes += '#'
    return f'r{hashes}"{t}"{hashes}'


def generate():
    meta = json.loads(cargo('metadata', '--format-version', '1', '--locked',
                            '--features', FEATURES, '--filter-platform', TARGET))
    by_key = {(p['name'], p['version']): p for p in meta['packages']}
    texts, groups = [], {}
    for key in sorted(shipped()):
        pkg = by_key[key]
        own = own_texts(pkg)
        ts = own or [standard_text(pkg)]
        idx = []
        for t in ts:
            if t not in texts:
                texts.append(t)
            idx.append(texts.index(t))
        groups.setdefault((tuple(idx), pkg.get('license') or ''), []).append(pkg)
    out = ['// Generated by scripts/gen-licenses.py from Cargo.lock: do not edit.',
           '// The Rust crates HushMic ships and their licence texts.',
           '',
           'use super::licenses::{Crate, CrateGroup};',
           '',
           'pub const GROUPS: &[CrateGroup] = &[']
    for (idx, lic), pkgs in sorted(groups.items(), key=lambda g: g[1][0]['name']):
        out.append('    CrateGroup {')
        out.append(f'        license: {json.dumps(lic)},')
        out.append('        crates: &[')
        for p in pkgs:
            repo = p.get('repository') or ''
            out.append(f'            Crate {{ name: {json.dumps(p["name"])}, version: '
                       f'{json.dumps(p["version"])}, source: {json.dumps(repo)} }},')
        out.append('        ],')
        out.append('        texts: &[' + ', '.join(f'TEXT_{i}' for i in idx) + '],')
        out.append('    },')
    out.append('];')
    for i, t in enumerate(texts):
        out.append('')
        out.append(f'const TEXT_{i}: &str = {rust_str(t)};')
    # as `cargo fmt` leaves it
    return subprocess.run(['rustfmt', '--edition', '2021', '--emit', 'stdout'],
                          input='\n'.join(out) + '\n', check=True,
                          capture_output=True, text=True).stdout


def main():
    text = generate()
    if '--check' in sys.argv:
        with open(OUT, encoding='utf-8') as fh:
            if fh.read() != text:
                sys.exit(f'{OUT} is stale: run scripts/gen-licenses.py')
        return
    with open(OUT, 'w', encoding='utf-8') as fh:
        fh.write(text)


if __name__ == '__main__':
    main()
