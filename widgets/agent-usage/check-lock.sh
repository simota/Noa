#!/bin/sh
# Every `name version` pair of widgets/agent-usage/Cargo.lock must also occur in
# the root Cargo.lock or another widgets/*/Cargo.lock (no new crate, no new version).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
pairs() { awk '/^name = /{gsub(/"/,"",$3);n=$3} /^version = /{gsub(/"/,"",$3);print n" "$3}' "$1"; }
pairs "$here/Cargo.lock" | LC_ALL=C sort -u > "${TMPDIR:-/tmp}/agent-usage-own.$$"
{ pairs "$root/Cargo.lock"; for f in "$root"/widgets/*/Cargo.lock; do
    [ "$f" = "$here/Cargo.lock" ] || pairs "$f"; done; } | LC_ALL=C sort -u > "${TMPDIR:-/tmp}/agent-usage-other.$$"
missing=$(LC_ALL=C comm -23 "${TMPDIR:-/tmp}/agent-usage-own.$$" "${TMPDIR:-/tmp}/agent-usage-other.$$" | grep -v '^agent-usage ' || true)
rm -f "${TMPDIR:-/tmp}/agent-usage-own.$$" "${TMPDIR:-/tmp}/agent-usage-other.$$"
if [ -n "$missing" ]; then echo "lock check FAILED, not in any other lock:"; echo "$missing"; exit 1; fi
echo "lock check ok"
