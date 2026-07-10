#!/usr/bin/env bash
# Validate the repository, create the version tag, and trigger the Release workflow.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DRY_RUN=false
ASSUME_YES=false

usage() {
  cat <<'USAGE'
Usage: scripts/release.sh [--dry-run] [--yes]

Options:
  --dry-run  Run every preflight check without creating or pushing a tag.
  --yes      Skip the final interactive confirmation.
  -h, --help Show this help.
USAGE
}

fail() {
  echo "error: $*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=true ;;
    --yes) ASSUME_YES=true ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; fail "unknown option: $1" ;;
  esac
  shift
done

for command in awk cargo git; do
  command -v "$command" >/dev/null 2>&1 || fail "required command not found: $command"
done

workspace_version="$(
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
cask_version="$(
  awk '/^[[:space:]]+version "/ {
    value = $2
    gsub(/"/, "", value)
    print value
    exit
  }' Casks/noa.rb
)"

[ -n "$workspace_version" ] || fail "unable to read the workspace version"
[[ "$workspace_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]] \
  || fail "workspace version is not valid SemVer: $workspace_version"
[ "$cask_version" = "$workspace_version" ] \
  || fail "Cask version must be $workspace_version; got $cask_version"

tag="v${workspace_version}"
branch="$(git symbolic-ref --quiet --short HEAD)" || fail "releases must run from a branch"
[ "$branch" = "main" ] || fail "releases must run from main; current branch is $branch"
[ -z "$(git status --porcelain --untracked-files=normal)" ] \
  || fail "working tree must be clean before releasing"

echo "Fetching origin/main and tags..."
git fetch --quiet origin main --tags
git show-ref --verify --quiet refs/remotes/origin/main \
  || fail "origin/main is unavailable"
[ "$(git rev-parse HEAD)" = "$(git rev-parse refs/remotes/origin/main)" ] \
  || fail "main must exactly match origin/main before releasing"

if git show-ref --verify --quiet "refs/tags/$tag"; then
  fail "tag already exists: $tag"
fi

echo "Running release checks for Noa $workspace_version..."
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked

echo
echo "Release preflight passed:"
echo "  version: $workspace_version"
echo "  tag:     $tag"
echo "  commit:  $(git rev-parse --short HEAD)"

if [ "$DRY_RUN" = true ]; then
  echo "Dry run complete; no tag was created or pushed."
  exit 0
fi

if [ "$ASSUME_YES" != true ]; then
  [ -t 0 ] || fail "interactive confirmation requires a terminal; use --yes in automation"
  read -r -p "Type $tag to create and push the release tag: " confirmation
  [ "$confirmation" = "$tag" ] || fail "release cancelled"
fi

git tag -a "$tag" -m "Noa $workspace_version"
if ! git push origin "$tag"; then
  echo "error: failed to push $tag; the local tag was kept for inspection" >&2
  echo "retry: git push origin $tag" >&2
  exit 1
fi

echo "Release workflow triggered for $tag."
echo "Track it at: https://github.com/simota/Noa/actions/workflows/release.yml"
