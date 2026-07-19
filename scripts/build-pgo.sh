#!/usr/bin/env bash
# Build a PGO-optimized release binary of noa.
#
# Profile-guided optimization on the pty→parser→grid ingest hot path,
# measured +4-6% bulk-output throughput over the plain release build
# (150MB_ascii +6%, synthetic mixed workload +4%, unicode neutral; M4,
# 2026-07). Profiles are collected headlessly via the noa-grid bench
# examples — no GUI or pty needed — then applied to the full `noa` build.
#
# Usage: scripts/build-pgo.sh
# Output: target/release/noa
#
# Note: the final stage rebuilds the workspace with -Cprofile-use, so
# `target/release` artifacts alternate flags with plain `cargo build
# --release` runs (full rebuild when switching between the two).

set -euo pipefail
cd "$(dirname "$0")/.."

# Honor an inherited CARGO_TARGET_DIR (bundle-macos.sh exports one) so all
# PGO artifacts land under the same target root as the final binary.
target_root="${CARGO_TARGET_DIR:-$(pwd)/target}"
case "${target_root}" in
  /*) ;;
  *) target_root="$(pwd)/${target_root}" ;;
esac

# ── locate llvm-profdata (rustup llvm-tools first, Xcode fallback) ──────
host="$(rustc -vV | awk '/^host:/ { print $2 }')"
profdata="$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-profdata"
if [[ ! -x "${profdata}" ]]; then
  profdata="$(xcrun -f llvm-profdata 2>/dev/null || true)"
fi
if [[ -z "${profdata}" || ! -x "${profdata}" ]]; then
  echo "llvm-profdata not found; run: rustup component add llvm-tools" >&2
  exit 1
fi

# ── ensure bench corpora exist (gitignored, generated) ──────────────────
if [[ ! -f bench/150MB_ascii.txt || ! -f bench/150MB_unicode.txt ]]; then
  (cd bench && python3 generate_data.py)
fi
if [[ ! -f bench/scroll_stress.txt ]]; then
  (cd bench && python3 gen_scroll.py)
fi

instr_dir="${target_root}/pgo-instr"
raw_dir="${target_root}/pgo-profiles"
merged="${target_root}/noa.profdata"
rm -rf "${raw_dir}"

# ── 1. instrumented build of the headless ingest benches ────────────────
echo "==> building instrumented benches"
CARGO_TARGET_DIR="${instr_dir}" \
RUSTFLAGS="-Cprofile-generate=${raw_dir}" \
  cargo build --release -p noa-grid \
    --example feed_bench --example bench_throughput

# ── 2. exercise the hot path with the standard bench corpora ────────────
echo "==> collecting profiles"
"${instr_dir}/release/examples/feed_bench" bench/150MB_ascii.txt 120 40 2 > /dev/null
"${instr_dir}/release/examples/feed_bench" bench/150MB_unicode.txt 120 40 2 > /dev/null
"${instr_dir}/release/examples/feed_bench" bench/scroll_stress.txt 120 40 2 > /dev/null
"${instr_dir}/release/examples/bench_throughput" > /dev/null

# ── 3. merge, 4. rebuild noa with the profile ───────────────────────────
echo "==> merging profiles"
"${profdata}" merge -o "${merged}" "${raw_dir}"/*.profraw

echo "==> building PGO-optimized noa"
RUSTFLAGS="-Cprofile-use=${merged}" cargo build --release -p noa

echo "==> done: target/release/noa"
