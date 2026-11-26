#!/usr/bin/env python3
"""Bake a font's GPOS pair kerning into a legacy `kern` table for egui.

egui (ab_glyph) reads only the old `kern` table; Geist keeps its kerning in
GPOS, so without this every line is about 2 % wider than the browser sets
it. This flattens the `kern` feature's pair lookups (formats 1 and 2) into
format-0 subtables, keeps the pairs a real word can hit (same script, or
with punctuation: never Latin next to Cyrillic), drops values under
--min units, and removes the layout tables egui ignores.

--liga writes Chrome's two-letter ligatures (ff, fi, fl, tt in Geist) as
kerning: the pair moves together by the ligature's advance minus the two
letters' (egui draws the two letters; the spacing then matches the browser,
which sets "effect" 1.4 px narrower at 13 px than f + f + kern).

--features ss01,tnum first applies those features' single substitutions to
the character map (the design system's components set `font-feature-settings:
"ss01", "tnum"`: Geist's alternate a and tabular figures), and --unicodes-from
REF.ttf then subsets the font to REF's characters, so the variant covers the
same text as the plain file; --kern-unicodes-from KREF keeps only the pairs
between KREF's characters (the window's Vietnamese letters are drawn but not
kerned: their pairs would double the table).

usage: bake-font-kerning.py IN.ttf OUT.ttf [--min 8] [--liga] [--features ss01,tnum]
                             [--unicodes-from REF.ttf]
(needs fontTools)

The window's files (IN from the Geist 1.800 static TTFs, REF the character
set the window covers):
  bake-font-kerning.py Geist-400.ttf Geist-400-kern.ttf --liga --unicodes-from REF+VI \
      --kern-unicodes-from REF
  (500 and 600 the same; the 500 component file adds --features tnum), where REF is
  Latin, Latin Extended-A, Cyrillic and punctuation, and REF+VI adds Vietnamese
  (U+1EA0-1EF9, U+01A0/01A1/01AF/01B0, combining U+0300/0301/0303/0309/0323).
"""
import argparse

from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.tables._k_e_r_n import KernTable_format_0


def gpos_pairs(f):
    gpos = f['GPOS'].table
    lookups = set()
    for fr in gpos.FeatureList.FeatureRecord:
        if fr.FeatureTag == 'kern':
            lookups.update(fr.Feature.LookupListIndex)
    pairs = {}
    order = f.getGlyphOrder()

    def add(a, b, v):
        if v and (a, b) not in pairs:
            pairs[(a, b)] = v

    for li in sorted(lookups):
        lk = gpos.LookupList.Lookup[li]
        subs = [s.ExtSubTable for s in lk.SubTable] if lk.LookupType == 9 else lk.SubTable
        for st in subs:
            fmt = getattr(st, 'Format', None)
            if fmt == 1:
                for i, first in enumerate(st.Coverage.glyphs):
                    for pvr in st.PairSet[i].PairValueRecord:
                        add(first, pvr.SecondGlyph, getattr(pvr.Value1, 'XAdvance', 0) if pvr.Value1 else 0)
            elif fmt == 2:
                c1, c2 = st.ClassDef1.classDefs, st.ClassDef2.classDefs
                by2 = {}
                for g, c in c2.items():
                    by2.setdefault(c, []).append(g)
                class0 = [g for g in order if g not in c2]
                for first in st.Coverage.glyphs:
                    rec = st.Class1Record[c1.get(first, 0)]
                    for k2, r2 in enumerate(rec.Class2Record):
                        v = getattr(r2.Value1, 'XAdvance', 0) if r2.Value1 else 0
                        if v:
                            for second in (by2.get(k2, []) if k2 else class0):
                                add(first, second, v)
    return pairs


def apply_features(f, tags):
    """Point the character map at the glyphs `tags` substitute (single substitutions)."""
    gsub = f['GSUB'].table
    lookups = set()
    for fr in gsub.FeatureList.FeatureRecord:
        if fr.FeatureTag in tags:
            lookups.update(fr.Feature.LookupListIndex)
    mapping = {}
    for li in sorted(lookups):
        for st in gsub.LookupList.Lookup[li].SubTable:
            st = getattr(st, 'ExtSubTable', st)
            for k, v in getattr(st, 'mapping', {}).items():
                mapping.setdefault(k, v)
    for t in f['cmap'].tables:
        t.cmap = {cp: mapping.get(g, g) for cp, g in t.cmap.items()}
    return mapping


def liga_pairs(f):
    """Two-letter ligatures of the `liga` feature as (first, second) -> advance change."""
    gsub = f['GSUB'].table
    lookups = set()
    for fr in gsub.FeatureList.FeatureRecord:
        if fr.FeatureTag == 'liga':
            lookups.update(fr.Feature.LookupListIndex)
    hm = f['hmtx']
    out = {}
    for li in sorted(lookups):
        for st in gsub.LookupList.Lookup[li].SubTable:
            st = getattr(st, 'ExtSubTable', st)
            for first, ligs in getattr(st, 'ligatures', {}).items():
                for lig in ligs:
                    if len(lig.Component) == 1:
                        second = lig.Component[0]
                        out[(first, second)] = hm[lig.LigGlyph][0] - hm[first][0] - hm[second][0]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('src')
    ap.add_argument('dst')
    ap.add_argument('--min', type=int, default=8)
    ap.add_argument('--features', default='')
    ap.add_argument('--liga', action='store_true')
    ap.add_argument('--unicodes-from')
    ap.add_argument('--kern-unicodes-from')
    a = ap.parse_args()
    f = TTFont(a.src)
    if a.features:
        print(len(apply_features(f, set(a.features.split(',')))), 'glyphs substituted')
    ligas = liga_pairs(f) if a.liga else {}
    if a.unicodes_from:
        from fontTools import subset
        opts = subset.Options()
        opts.layout_features = ['kern']
        opts.name_IDs = ['*']
        sub = subset.Subsetter(opts)
        sub.populate(unicodes=TTFont(a.unicodes_from).getBestCmap().keys())
        sub.subset(f)
    cmap = f.getBestCmap()
    script = {}
    for cp, g in cmap.items():
        s = 'cyr' if 0x400 <= cp <= 0x52F else 'lat' if chr(cp).isalpha() else 'punct'
        script.setdefault(g, s)
    def kept(pair, v):
        return abs(v) >= a.min and {script.get(pair[0], 'punct'), script.get(pair[1], 'punct')} != {'cyr', 'lat'}

    pairs = {k: v for k, v in gpos_pairs(f).items() if kept(k, v)}
    if a.kern_unicodes_from:
        allowed = set(TTFont(a.kern_unicodes_from).getBestCmap())
        kerned = {g for cp, g in cmap.items() if cp in allowed}
        pairs = {k: v for k, v in pairs.items() if k[0] in kerned and k[1] in kerned}
    order = set(f.getGlyphOrder())
    for k, v in ligas.items():
        if k[0] in order and k[1] in order:
            pairs[k] = v
    kern = newTable('kern')
    kern.version = 0
    kern.kernTables = []
    items = sorted(pairs.items(), key=lambda kv: (f.getGlyphID(kv[0][0]), f.getGlyphID(kv[0][1])))
    for i in range(0, len(items), 10000):
        st = KernTable_format_0()
        st.version, st.coverage, st.format = 0, 1, 0
        st.kernTable = dict(items[i:i + 10000])
        kern.kernTables.append(st)
    f['kern'] = kern
    for tag in ('GPOS', 'GSUB', 'GDEF'):
        if tag in f:
            del f[tag]
    f.save(a.dst)
    print(a.dst, len(pairs), 'pairs')


if __name__ == '__main__':
    main()
