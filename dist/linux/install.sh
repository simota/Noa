#!/usr/bin/env sh
# Install (or uninstall) Noa from an unpacked release tarball.
#
#   ./install.sh                     # install into ~/.local
#   ./install.sh --prefix /usr/local # system-wide (run with sudo)
#   ./install.sh --uninstall [--prefix DIR]
set -eu

APP_ID="com.simota.noa"
PREFIX="${HOME}/.local"
UNINSTALL=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift ;;
    --prefix=*) PREFIX="${1#--prefix=}" ;;
    --uninstall) UNINSTALL=true ;;
    -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

SRC="$(cd "$(dirname "$0")" && pwd)"
FILES="bin/Noa share/applications/$APP_ID.desktop share/metainfo/$APP_ID.metainfo.xml"
for icon in "$SRC"/share/icons/hicolor/*/apps/"$APP_ID.png"; do
  FILES="$FILES ${icon#"$SRC"/}"
done

for f in $FILES; do
  if [ "$UNINSTALL" = true ]; then
    rm -f "$PREFIX/$f"
  else
    mkdir -p "$(dirname "$PREFIX/$f")"
    cp -f "$SRC/$f" "$PREFIX/$f"
  fi
done

# Point the launcher at the installed binary so it works even when
# $PREFIX/bin is not on the session PATH (common for ~/.local/bin).
if [ "$UNINSTALL" = false ]; then
  sed -i "s|^Exec=Noa|Exec=$PREFIX/bin/Noa|" "$PREFIX/share/applications/$APP_ID.desktop"
fi

# Refresh desktop caches when the tools exist; harmless when they do not.
command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database -q "$PREFIX/share/applications" 2>/dev/null || true
# Only refresh an icon cache that already exists: creating one in a user
# prefix would hide icons other apps later install without refreshing it.
[ -f "$PREFIX/share/icons/hicolor/icon-theme.cache" ] \
  && command -v gtk-update-icon-cache >/dev/null 2>&1 \
  && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true

if [ "$UNINSTALL" = true ]; then
  echo "Removed Noa from $PREFIX"
else
  echo "Installed Noa into $PREFIX"
  case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "note: $PREFIX/bin is not on PATH" ;;
  esac
fi
