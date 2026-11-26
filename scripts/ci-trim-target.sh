#!/usr/bin/env bash
# Trim the shared target directory before CI saves it as a cache.
#
# CI caches the target directory between runs so that the registry
# dependencies of the workspace and of the widget crates are not compiled from
# scratch every time. Noa's own packages (the workspace members and the widget
# crates) are rebuilt on every run anyway: a fresh checkout makes their sources
# newer than any cached artifact. Their artifacts, the final binaries and the
# incremental state are therefore dropped, which keeps the cache small enough
# to restore quickly and to fit the repository's cache quota.
#
# This is not a check and changes nothing that scripts/verify.sh tests.
set -euo pipefail
cd "$(dirname "$0")/.."
target=${CARGO_TARGET_DIR:-target}
[ -d "$target/debug" ] || exit 0

for m in Cargo.toml widgets/*/Cargo.toml; do
  cargo metadata --manifest-path "$m" --no-deps --format-version 1 --offline
done | python3 -I -c '
import json, os, re, shutil, sys

def size(path):
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            try: total += os.lstat(os.path.join(root, f)).st_size
            except OSError: pass
    return total

# The names of Noa own packages and their targets, from the manifests.
dec = json.JSONDecoder(); s = sys.stdin.read(); i = 0; names = set()
while i < len(s):
    if s[i].isspace(): i += 1; continue
    doc, i = dec.raw_decode(s, i)
    for p in doc["packages"]:
        for n in [p["name"]] + [t["name"] for t in p["targets"]]:
            names.update({n, n.replace("-", "_")})

target = sys.argv[1]; profile = os.path.join(target, "debug")
before = size(target)
def remove(path):
    if os.path.isdir(path) and not os.path.islink(path): shutil.rmtree(path)
    elif os.path.lexists(path): os.remove(path)

# Final binaries, examples and incremental state: rebuilt on every run.
for e in os.scandir(profile):
    if e.is_file(follow_symlinks=False) or e.name in ("examples", "incremental"):
        remove(e.path)
# A unit is <name>-<16 hex digits>[.ext]; the name must match exactly, so a
# registry crate whose name merely starts with one of Noa is kept.
unit = re.compile(r"(.+)-[0-9a-f]{16}(\..*)?$")
for sub in ("deps", ".fingerprint", "build"):
    d = os.path.join(profile, sub)
    if not os.path.isdir(d): continue
    for e in os.scandir(d):
        m = unit.match(e.name)
        if m and m.group(1) in names: remove(e.path)
print(f"ci-trim-target: {before >> 20} MiB -> {size(target) >> 20} MiB")
' "$target"
