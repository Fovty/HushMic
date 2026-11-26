#!/usr/bin/env python3
"""packaging/flatpak/cargo-sources.json from Cargo.lock, offline.

The same output as flatpak-cargo-generator for a lockfile whose packages
all come from crates.io (no git dependencies): one archive and one
.cargo-checksum.json per crate, then the vendored-sources config.

usage: cargo-sources.py Cargo.lock packaging/flatpak/cargo-sources.json
"""
import json, sys, tomllib
lock = tomllib.load(open(sys.argv[1], 'rb'))
out = []
for p in lock['package']:
    if p.get('source') != 'registry+https://github.com/rust-lang/crates.io-index':
        continue
    n, v, c = p['name'], p['version'], p['checksum']
    dest = f'cargo/vendor/{n}-{v}'
    out.append({"type": "archive", "archive-type": "tar-gzip", "url": f"https://static.crates.io/crates/{n}/{n}-{v}.crate", "sha256": c, "dest": dest})
    out.append({"type": "inline", "contents": json.dumps({"package": c, "files": {}}), "dest": dest, "dest-filename": ".cargo-checksum.json"})
out.append({"type": "inline", "contents": "[source.vendored-sources]\ndirectory = \"cargo/vendor\"\n\n[source.crates-io]\nreplace-with = \"vendored-sources\"\n", "dest": "cargo", "dest-filename": "config"})
if any(p.get('source', '').startswith('git+') for p in lock['package']):
    sys.exit('git dependencies: use flatpak-cargo-generator')
json.dump(out, open(sys.argv[2], 'w'), indent=4)
