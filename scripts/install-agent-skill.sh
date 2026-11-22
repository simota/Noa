#!/bin/sh
# Install this repository's agent skills (every skills/*/SKILL.md) by linking
# each one into the skills directory of Claude Code, Codex and agy, so they
# follow this checkout as it updates.
#
#   scripts/install-agent-skill.sh                 # claude, codex and agy
#   scripts/install-agent-skill.sh --agent codex   # only that agent (repeatable)
#   scripts/install-agent-skill.sh --dir DIR       # only DIR (e.g. a project's .claude/skills)
#   scripts/install-agent-skill.sh --uninstall     # remove the links again
#
# Skills directories: claude ~/.claude/skills, codex ~/.codex/skills,
# agy ~/.gemini/antigravity-cli/skills. An agent whose home directory
# (~/.claude, ~/.codex, ~/.gemini/antigravity-cli) does not exist is skipped.
# Anything already at a destination that is not this link is left alone.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd -P)
agents=""
dirs=""
uninstall=0

agent_dir() {
  case $1 in
    claude) echo "$HOME/.claude/skills" ;;
    codex) echo "$HOME/.codex/skills" ;;
    agy) echo "$HOME/.gemini/antigravity-cli/skills" ;;
    *) echo "unknown agent: $1 (claude, codex or agy)" >&2; exit 2 ;;
  esac
}

while [ $# -gt 0 ]; do
  case $1 in
    --agent) [ $# -ge 2 ] || { echo "--agent needs a name" >&2; exit 2; }; agent_dir "$2" >/dev/null; agents="$agents $2"; shift 2 ;;
    --dir) [ $# -ge 2 ] || { echo "--dir needs a directory" >&2; exit 2; }; dirs="$dirs
$2"; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    -h | --help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# With neither --agent nor --dir, every agent installed on this machine.
if [ -z "$agents" ] && [ -z "$dirs" ]; then
  agents="claude codex agy"
  skip_missing=1
else
  skip_missing=0
fi
for agent in $agents; do
  dir=$(agent_dir "$agent")
  if [ "$skip_missing" = 1 ] && [ ! -d "$(dirname "$dir")" ]; then
    echo "skip $agent ($(dirname "$dir") not found)"
    continue
  fi
  dirs="$dirs
$dir"
done

skills=$(for f in "$here"/skills/*/SKILL.md; do [ -f "$f" ] && basename "$(dirname "$f")"; done)
[ -n "$skills" ] || { echo "no skills found under $here/skills" >&2; exit 1; }

status=0
old_ifs=$IFS
IFS='
'
for dir in $dirs; do
  [ -n "$dir" ] || continue
  for name in $skills; do
    source="$here/skills/$name"
    link="$dir/$name"
    if [ -L "$link" ] && [ "$(readlink "$link")" = "$source" ]; then
      if [ "$uninstall" = 1 ]; then
        rm "$link"
        echo "removed $link"
      else
        echo "already installed: $link -> $source"
      fi
    elif [ -e "$link" ] || [ -L "$link" ]; then
      echo "$link already exists and is not a link to $source; left alone" >&2
      status=1
    elif [ "$uninstall" = 1 ]; then
      echo "$link is not installed"
    else
      mkdir -p "$dir"
      ln -s "$source" "$link"
      echo "installed: $link -> $source"
    fi
  done
done
IFS=$old_ifs
exit "$status"
