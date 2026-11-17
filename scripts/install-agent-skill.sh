#!/bin/sh
# Install the noa-orchestration agent skill by linking it into a skills
# directory, so it follows this checkout as it updates.
#
#   scripts/install-agent-skill.sh               # ~/.claude/skills/noa-orchestration
#   scripts/install-agent-skill.sh --dir DIR     # DIR/noa-orchestration
#   scripts/install-agent-skill.sh --uninstall   # remove the link again
#
# Anything already at the destination that is not this link is left alone.
set -eu

name=noa-orchestration
here=$(cd "$(dirname "$0")/.." && pwd -P)
source="$here/skills/$name"
dir="$HOME/.claude/skills"
uninstall=0

while [ $# -gt 0 ]; do
  case $1 in
    --dir) [ $# -ge 2 ] || { echo "--dir needs a directory" >&2; exit 2; }; dir=$2; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    -h | --help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

link="$dir/$name"
ours() { [ -L "$link" ] && [ "$(readlink "$link")" = "$source" ]; }

if [ "$uninstall" = 1 ]; then
  if ours; then
    rm "$link"
    echo "removed $link"
  elif [ -e "$link" ] || [ -L "$link" ]; then
    echo "$link is not a link to $source; left alone" >&2
    exit 1
  else
    echo "$link is not installed"
  fi
  exit 0
fi

[ -f "$source/SKILL.md" ] || { echo "skill not found at $source" >&2; exit 1; }
if ours; then
  echo "already installed: $link -> $source"
  exit 0
fi
if [ -e "$link" ] || [ -L "$link" ]; then
  echo "$link already exists and is not a link to $source; remove it first" >&2
  exit 1
fi
mkdir -p "$dir"
ln -s "$source" "$link"
echo "installed: $link -> $source"
