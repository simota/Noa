#!/bin/sh
# Every `name version` pair of widgets/agent-usage/Cargo.lock must also occur in
# the root Cargo.lock or another widgets/*/Cargo.lock (no new crate, no new version).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/agent-usage-lock.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
# A lock that cannot be read fails the check: never compare against an empty set.
pairs() {
    [ -r "$1" ] || { echo "lock check FAILED, cannot read $1" >&2; exit 1; }
    awk '/^name = /{gsub(/"/,"",$3);n=$3} /^version = /{gsub(/"/,"",$3);print n" "$3}' "$1" > "$tmp/raw" \
        || { echo "lock check FAILED, cannot parse $1" >&2; exit 1; }
    cat "$tmp/raw"
}
pairs "$here/Cargo.lock" > "$tmp/own.raw"
LC_ALL=C sort -u "$tmp/own.raw" > "$tmp/own"
: > "$tmp/other.raw"
pairs "$root/Cargo.lock" >> "$tmp/other.raw"
for f in "$root"/widgets/*/Cargo.lock; do
    [ "$f" = "$here/Cargo.lock" ] && continue
    pairs "$f" >> "$tmp/other.raw"
done
LC_ALL=C sort -u "$tmp/other.raw" > "$tmp/other"
missing=$(LC_ALL=C comm -23 "$tmp/own" "$tmp/other" | grep -v '^agent-usage ' || true)
if [ -n "$missing" ]; then echo "lock check FAILED, not in any other lock:"; echo "$missing"; exit 1; fi
echo "lock check ok"
