# Authoritative post-merge head-to-head — `perf/fastest-terminal`

Machine: Apple M4 (10 cores), macOS 26.5.1, arm64. noa 0.1.4 release of
`perf/fastest-terminal` (all 3 optimization tracks merged). Load < 4 before each
axis group. Equalized (120×40 / Menlo 14, no ligatures/bg) where pinnable;
latency/startup are condition-independent.

## Full 4-axis table (noa vs competitors)

| Axis (unit, better) | noa | Ghostty | Termy | kitty |
|---|---|---|---|---|
| Throughput ASCII (MiB/s ↑) | 138.5 | 70.7 | **143.4** | 103.6 |
| Throughput Unicode (MiB/s ↑) | 148.6 | 78.3 | **150.1** | 114.7 |
| Scroll median / best (ms ↓) | 328 / 292 | 973 / 945 | **273 / 265** | 380 / 367 |
| Latency median (µs ↓, clean) | **22** | 33 | 16664 | 3814 |
| Latency p99 (µs ↓, clean) | 74 | **62** | 36957 | 4083 |
| Startup (ms ↓) | **91** | 173 | 199 | 289 |

Latency for noa/Ghostty is the fair **no-activation** number (`.latency_clean`
in results.json); the raw `.axes.latency` for noa (29/167) was inflated by the
harness focus-activation step, which injects tail jitter into off-thread
responders. Termy/kitty *require* activation to answer DSR at all, so their
numbers keep it. Scroll "best" = min of 6–8 reps (noa is CPU-bound/variable;
Termy/kitty are display-paced so median≈best).

## noa vs its own history (delta)

| Axis | baseline 010934 | eq-baseline 012100 | **post-merge** | Δ vs baseline |
|---|---|---|---|---|
| Throughput ASCII | 126.2 | 127.1 | **138.5** | +12.3 (+9.7%) ✅ |
| Throughput Unicode | 118.1 | 121.0 | **148.6** | +30.5 (+25.8%) ✅ |
| Scroll (best ms) | 184 | 169 | **292** | +108 ms (**REGRESSED** ❌) |
| Latency median (µs) | 18 | — | **22** | +4 µs (slightly worse) |
| Latency p99 (µs) | 54 | — | **74** | +20 µs (worse) |
| Startup (ms) | 254 | — | **91** | −163 ms (−64%) ✅✅ |

## Verdict per axis — does noa beat BOTH Ghostty AND Termy?

1. **Throughput ASCII** — ❌ NO. Beats Ghostty (138.5 vs 70.7) and kitty, but
   loses to **Termy 143.4** (−3.4%). Improved +9.7% from baseline; still short.
2. **Throughput Unicode** — ❌ NO (narrow). 148.6 vs Ghostty 78.3 (win); vs
   **Termy 150.1** (−1.0%). Nearly caught Termy (+25.8% gain) but not past it.
3. **Frame/Scroll** — ❌ NO, and **REGRESSED**. noa best 292 ms beats Ghostty
   (945) but loses to **Termy 265 ms** (−10%). noa held #1 here in both prior
   runs (184/169 ms); the merge pushed it to #2. Highest-priority gap.
4. **Input latency (median)** — ✅ YES. 22 µs beats Ghostty 33 and Termy 16664.
   **Latency (p99)** — ❌ NO vs Ghostty: noa 74 µs vs **Ghostty 62 µs** (tail is
   ~19% worse). Beats Termy p99 (36957) easily. So median-win, p99-miss.
5. **Warm startup** — ✅ YES, decisively. 91 ms beats Ghostty 173, Termy 199,
   kitty 289. Cleanest win of the run (−64% vs baseline, ~matches the ~81 ms
   track target).

### Summary
- **Beats BOTH competitors:** startup (decisive), latency-median.
- **Gaps for the next convergence cycle:**
  - **Scroll regression** (❌ + went backwards): noa 292 ms vs Termy 265 ms.
    Suspect Team B's off-thread scrollback packer / deferred batch seal adding
    per-scroll-region overhead on the SGR+DECSTBM workload. Confirmed real
    (stable reps, quiet load, both default and equalized config).
  - **Throughput vs Termy** (❌ narrow): ASCII −3.4%, Unicode −1.0%. The
    headless "268/234 MiB/s grid" gains don't fully land in the full-app
    flow-control-bounded consume path; present/render pacing caps it.
  - **Latency p99 vs Ghostty** (❌ narrow): 74 vs 62 µs — noa's tail, not its
    median, is the miss.

## Bugs found
- **noa `-e` does not run the command for the first window.** `--help` advertises
  it; `PtyConfig.command` is plumbed, but the pre-booted startup shell (Team C
  fast-start) consumes the first pane and ignores `launch_command`. Repro:
  `noa -e /bin/sh -c 'echo hi > /tmp/x'` writes nothing. The harness fell back to
  the `$SHELL`-wrapper path (same mechanism as all prior baselines) to measure.
