#!/usr/bin/env bash
# One command for a person, an agent and CI.
#   scripts/verify.sh          the checks that reach what changed since main
#   scripts/verify.sh --full   every check, as CI runs it
# Each check's output is in .verify/<check>.log, its time in .verify/timings.
# Exits 1 and names every check that failed.
#
# Sandboxed agents: export CARGO_NET_OFFLINE=true (dependencies are already
# fetched), and run with the sandbox disabled when noa-pty or noa-ipc tests are
# reached: they need openpty and a loopback bind.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p .verify && : >.verify/timings

# CI installs 1.92.0 and makes it the default. Pin the same toolchain here so a
# newer local `stable` (with newer lints) does not disagree with CI.
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.92.0}"
os=$(uname -s)

failed=()
run() {                       # run <name> <command> [args...]
  local name=$1 start=$SECONDS log=".verify/$1.log"; shift
  printf '== %s\n' "$name"
  if "$@" >"$log" 2>&1; then
    printf '   ok\n'
  else
    printf '   FAILED: see %s\n' "$log"
    tail -n 40 "$log"
    failed+=("$name")
  fi
  printf '%s\t%ss\n' "$name" "$((SECONDS - start))" >>.verify/timings
}

# A changed path matching this, or a deleted one, reaches code no related test
# names: the gate itself, the workspace manifests and lockfile, vendored crates.
widens='^(scripts/verify\.sh|\.github/|Cargo\.(toml|lock)$|vendor/|\.cargo/|rust-toolchain)'

# Changed path -> the workspace package that owns or embeds it (include_str!).
package_of() {
  case $1 in
    crates/*/*) local d=${1#crates/}; echo "${d%%/*}" ;;
    bin/noa/*) echo noa ;;
    tests/parity/*) echo noa-parity ;;
    bench/probes/drain-staircase/*) echo drain-staircase ;;
    shell-integration/*) echo noa-pty ;;
    scripts/gen-themes) echo noa-theme ;;
    docs/AGENT_WORKFLOW.md | docs/CONFIGURATION.md | assets/Noa.sdef) echo noa-app ;;
  esac
}

# Only noa-app and noa-render may depend on wgpu; only noa-app on winit.
# Read from the manifests as declared, so no lockfile resolution is involved.
boundaries() {
  cargo metadata --no-deps --format-version 1 | python3 -I -c '
import json, sys
allowed = {"wgpu": {"noa-app", "noa-render"}, "winit": {"noa-app"}}
bad = [(d["name"], p["name"]) for p in json.load(sys.stdin)["packages"]
       for d in p["dependencies"] if d["name"] in allowed and p["name"] not in allowed[d["name"]]]
for dep, pkg in bad:
    print(f"{pkg} depends on {dep}: only {sorted(allowed[dep])} may")
sys.exit(1 if bad else 0)'
}

# widgets/agent-dashboard, widgets/repo-browser, widgets/discussion,
# widgets/competition and widgets/review are crates of their own, outside the
# workspace. Each is
# built with its tests everywhere; the tests run on macOS only, because some
# expect the text widths of macOS's system fonts.
dashboard=widgets/agent-dashboard/Cargo.toml
browser=widgets/repo-browser/Cargo.toml
discussion=widgets/discussion/Cargo.toml
competition=widgets/competition/Cargo.toml
review=widgets/review/Cargo.toml
widget_check() {
  cargo build --manifest-path "$1" --locked --all-targets || return
  [ "$os" = Darwin ] || return 0
  cargo test --manifest-path "$1" --locked
}
# The packages a widget is built from by path: itself and the Noa crates it uses.
widget_uses() {
  cargo tree --manifest-path "$1" --locked -e normal --prefix none -f '{p}' |
    awk '/ \(\// {print $1}' | sort -u
}

full=0 changed=(); [ "${1:-}" = "--full" ] && full=1
[ "$full" = 0 ] && while IFS= read -r path; do
  if [ ! -e "$path" ] || [[ $path =~ $widens ]]; then echo "verify: $path widens the change"; full=1; break; fi
  changed+=("$path")
done < <({ git diff --name-only "${VERIFY_BASE:-$(git merge-base HEAD main)}" --; git ls-files --others --exclude-standard; } | sort -u)

run format cargo fmt --all -- --check
run boundaries boundaries

if [ "$full" = 1 ]; then
  # CI gates clippy on Linux only: on macOS, Linux-only code is dead and
  # `-D warnings` fails on it.
  # #TODO(agent): OUT-OF-SCOPE — macOS clippy -D warnings is red on main (8 dead_code errors in noa-app linux_tabs.rs, quick_terminal.rs, text_panel.rs); gate it here once fixed.
  [ "$os" = Linux ] && run clippy cargo clippy --workspace --all-targets --locked -- -D warnings
  run build cargo build --workspace --locked
  run test cargo test --workspace --locked
  run dashboard widget_check "$dashboard"
  run browser widget_check "$browser"
  run discussion widget_check "$discussion"
  run competition widget_check "$competition"
  run review widget_check "$review"
  run hook-test python3 -I -B scripts/test_noa_agent_hook.py
  run skill-install-test python3 -I -B scripts/test_install_agent_skill.py
  if [ "$os" = Linux ]; then
    run desktop-files sh -c 'desktop-file-validate dist/linux/com.simota.noa.desktop &&
      appstreamcli validate --no-net dist/linux/com.simota.noa.metainfo.xml'
  fi
elif [ "${#changed[@]}" -gt 0 ]; then
  pkgs=() hook=0 skill=0 dash=0 rb=0 disc=0 comp=0 rev=0
  for path in "${changed[@]}"; do
    p=$(package_of "$path"); [ -n "$p" ] && pkgs+=("$p")
    case $path in widgets/agent-dashboard/*) dash=1 ;; esac
    case $path in widgets/repo-browser/*) rb=1 ;; esac
    case $path in widgets/discussion/*) disc=1 ;; esac
    case $path in widgets/competition/*) comp=1 ;; esac
    case $path in widgets/review/*) rev=1 ;; esac
    case $path in scripts/noa-agent-hook.py | scripts/test_noa_agent_hook.py) hook=1 ;; esac
    case $path in scripts/install-agent-skill.sh | scripts/test_install_agent_skill.py | skills/*) skill=1 ;; esac
  done
  if [ "${#pkgs[@]}" -gt 0 ]; then
    # The changed packages plus every workspace package that depends on them.
    related=$(for p in $(printf '%s\n' "${pkgs[@]}" | sort -u); do
      cargo tree --workspace --locked -i "$p" -e normal,dev --prefix none -f '{p}' 2>/dev/null | awk '{print $1}'
    done | sort -u)
    args=(); for p in $related; do args+=(-p "$p"); done
    echo "verify: related packages: $(echo $related)"
    [ "$os" = Linux ] && run clippy cargo clippy --locked --all-targets "${args[@]}" -- -D warnings
    run test cargo test --locked "${args[@]}"
    # Without a widget's package list, run its check rather than skip it.
    if uses=$(widget_uses "$dashboard"); then
      for p in "${pkgs[@]}"; do grep -qx "$p" <<<"$uses" && dash=1; done
    else
      echo "verify: cannot list the dashboard's packages; checking it anyway"
      dash=1
    fi
    if uses=$(widget_uses "$browser"); then
      for p in "${pkgs[@]}"; do grep -qx "$p" <<<"$uses" && rb=1; done
    else
      echo "verify: cannot list the repo browser's packages; checking it anyway"
      rb=1
    fi
    if uses=$(widget_uses "$discussion"); then
      for p in "${pkgs[@]}"; do grep -qx "$p" <<<"$uses" && disc=1; done
    else
      echo "verify: cannot list the discussion's packages; checking it anyway"
      disc=1
    fi
    if uses=$(widget_uses "$competition"); then
      for p in "${pkgs[@]}"; do grep -qx "$p" <<<"$uses" && comp=1; done
    else
      echo "verify: cannot list the competition's packages; checking it anyway"
      comp=1
    fi
    if uses=$(widget_uses "$review"); then
      for p in "${pkgs[@]}"; do grep -qx "$p" <<<"$uses" && rev=1; done
    else
      echo "verify: cannot list the review's packages; checking it anyway"
      rev=1
    fi
  fi
  [ "$dash" = 1 ] && run dashboard widget_check "$dashboard"
  [ "$rb" = 1 ] && run browser widget_check "$browser"
  [ "$disc" = 1 ] && run discussion widget_check "$discussion"
  [ "$comp" = 1 ] && run competition widget_check "$competition"
  [ "$rev" = 1 ] && run review widget_check "$review"
  [ "$hook" = 1 ] && run hook-test python3 -I -B scripts/test_noa_agent_hook.py
  [ "$skill" = 1 ] && run skill-install-test python3 -I -B scripts/test_install_agent_skill.py
fi

summary="verify: ok"; [ "${#failed[@]}" -gt 0 ] && summary="verify: FAILED: ${failed[*]}"
[ "$full" = 1 ] && [ -z "$(git status --porcelain)" ] && echo "$summary" >".verify/baseline-$(git rev-parse HEAD)"
not_covered="GUI behaviour and visual parity (by hand), GPU tests when no adapter is present (they skip)"
[ "$os" = Linux ] || not_covered="clippy and desktop-file validation (Linux only, as in CI), $not_covered"
[ "$os" = Darwin ] || not_covered="the dashboard's tests (macOS only; it is built), $not_covered"
echo "not covered: $not_covered"
echo "$summary"
[ "${#failed[@]}" -eq 0 ]
