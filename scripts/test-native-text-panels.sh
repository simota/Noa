#!/bin/bash
# Package synthetic GUI checks so LaunchServices can activate their native windows.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_dir"
cargo build -p noa-app --example native-text-panels
smoke_dir="$(mktemp -d "${TMPDIR:-/tmp}/noa-text-panels.XXXXXX")"
trap 'rm -rf "$smoke_dir"' EXIT
smoke_app="$smoke_dir/NoaPanelSmoke.app"
mkdir -p "$smoke_app/Contents/MacOS"
cp target/debug/examples/native-text-panels "$smoke_app/Contents/MacOS/NoaPanelSmoke"
cat > "$smoke_app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>NoaPanelSmoke</string>
<key>CFBundleIdentifier</key><string>org.noa.panel-smoke</string>
<key>CFBundleExecutable</key><string>NoaPanelSmoke</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
open -W -n "$smoke_app" --stdout "$smoke_dir/stdout" --stderr "$smoke_dir/stderr"
cat "$smoke_dir/stdout" "$smoke_dir/stderr"
# open reports successful launching even when the child process failed an assertion.
grep -Fq 'Native composer, Japanese draft, reader, find routing, and close checks passed.' "$smoke_dir/stdout"
