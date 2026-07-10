#!/usr/bin/env bash
# Build Noa and assemble a double-clickable macOS .app bundle.
#
#   scripts/bundle-macos.sh            # release build -> target/release/Noa.app
#   scripts/bundle-macos.sh debug      # debug build   -> target/debug/Noa.app
#
# No external tooling required (no cargo-bundle): assembles the bundle by hand,
# generates the app icon if missing, and ad-hoc code-signs so it launches
# without a Gatekeeper prompt on this machine.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODE="${1:-release}"
BUNDLE_ID="com.simota.noa"

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

TARGET_ROOT="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_ROOT" in
  /*) ;;
  *) TARGET_ROOT="$ROOT/$TARGET_ROOT" ;;
esac
export CARGO_TARGET_DIR="$TARGET_ROOT"

case "$MODE" in
  release) cargo build --release -p noa; PROFILE="release" ;;
  debug)   cargo build -p noa;           PROFILE="debug"   ;;
  *) echo "usage: $0 [release|debug]" >&2; exit 2 ;;
esac

TARGET_DIR="$TARGET_ROOT/$PROFILE"
BIN="$TARGET_DIR/Noa"
APP="$TARGET_DIR/Noa.app"
CONTENTS="$APP/Contents"

[ -x "$BIN" ] || { echo "error: binary not found at $BIN" >&2; exit 1; }

# Generate the icon on first run (best effort — the app still bundles without it).
if [ ! -f "$ROOT/assets/noa.icns" ]; then
  "$ROOT/scripts/gen-icon.sh" || echo "warning: icon generation failed; bundling without an icon" >&2
fi

rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$BIN" "$CONTENTS/MacOS/Noa"

ICON_KEY=""
if [ -f "$ROOT/assets/noa.icns" ]; then
  cp "$ROOT/assets/noa.icns" "$CONTENTS/Resources/noa.icns"
  ICON_KEY="
    <key>CFBundleIconFile</key>
    <string>noa</string>"
fi

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Noa</string>
    <key>CFBundleDisplayName</key>
    <string>Noa</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleExecutable</key>
    <string>Noa</string>${ICON_KEY}
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
PLIST

printf 'APPL????' > "$CONTENTS/PkgInfo"

# Ad-hoc code signature so double-clicking / `open` doesn't trip Gatekeeper.
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 \
  || echo "warning: ad-hoc codesign failed (app still runnable via terminal)" >&2

echo "Built $APP"
echo "Run it:  open \"$APP\"   (or double-click in Finder)"
