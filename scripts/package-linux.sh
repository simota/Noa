#!/usr/bin/env bash
# Build Noa and assemble a relocatable Linux tarball.
#
#   scripts/package-linux.sh            # release build -> target/release/Noa-<ver>-linux-<arch>.tar.gz
#   scripts/package-linux.sh debug      # debug build   -> target/debug/Noa-<ver>-linux-<arch>.tar.gz
#
# The tarball unpacks to a single Noa-<ver>-linux-<arch>/ directory laid out as
# an XDG prefix (bin/, share/applications, share/icons/hicolor, share/metainfo)
# plus an install.sh that copies it into a prefix (default ~/.local).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODE="${1:-release}"
APP_ID="com.simota.noa"

WORKSPACE_VERSION="$(
  awk '
    /^\[workspace\.package\]$/ { found = 1; next }
    /^\[/ { found = 0 }
    found && /^version[[:space:]]*=/ {
      value = $0
      sub(/^[^"]*"/, "", value)
      sub(/".*$/, "", value)
      print value
      exit
    }
  ' Cargo.toml
)"
VERSION="${NOA_VERSION:-$WORKSPACE_VERSION}"
[ -n "$VERSION" ] || {
  echo "error: unable to determine the app version" >&2
  exit 1
}

ARCH="$(uname -m)"
TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_ROOT" in
  /*) ;;
  *) TARGET_ROOT="$ROOT/$TARGET_ROOT" ;;
esac
export CARGO_TARGET_DIR="$TARGET_ROOT"

case "$MODE" in
  release)
    cargo build --release -p noa
    PROFILE_DIR="$TARGET_ROOT/release"
    ;;
  debug)
    cargo build -p noa
    PROFILE_DIR="$TARGET_ROOT/debug"
    ;;
  *)
    echo "usage: $0 [release|debug]" >&2
    exit 2
    ;;
esac

NAME="Noa-${VERSION}-linux-${ARCH}"
STAGE="$PROFILE_DIR/$NAME"
ARCHIVE="$PROFILE_DIR/$NAME.tar.gz"

rm -rf "$STAGE" "$ARCHIVE"
install -Dm755 "$PROFILE_DIR/Noa" "$STAGE/bin/Noa"
install -Dm644 "dist/linux/$APP_ID.desktop" "$STAGE/share/applications/$APP_ID.desktop"
install -Dm644 "dist/linux/$APP_ID.metainfo.xml" "$STAGE/share/metainfo/$APP_ID.metainfo.xml"
for icon in dist/linux/icons/*/"$APP_ID.png"; do
  size="$(basename "$(dirname "$icon")")"
  install -Dm644 "$icon" "$STAGE/share/icons/hicolor/$size/apps/$APP_ID.png"
done
install -Dm644 LICENSE "$STAGE/LICENSE"
install -Dm755 dist/linux/install.sh "$STAGE/install.sh"

if [ "$MODE" = release ]; then
  strip "$STAGE/bin/Noa" 2>/dev/null || true
fi

# Reproducible-ish archive: fixed mtimes, sorted entries, numeric owner.
tar --sort=name --mtime='1980-01-01 00:00:00Z' --owner=0 --group=0 --numeric-owner \
  -C "$PROFILE_DIR" -czf "$ARCHIVE" "$NAME"
(cd "$PROFILE_DIR" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")

echo "$ARCHIVE"
