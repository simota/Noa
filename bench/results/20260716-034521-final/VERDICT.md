# Decisive run v3 (FINAL) — `perf/fastest-terminal` HEAD 3f45bd4

Apple M4 (10 cores), macOS 26.5.1, arm64. noa release built from the branch with
the `-e` fix + all optimization/fix tracks. Quiet machine (load < 4, verified per
axis group). Equalized 120×40 / Menlo 14 where pinnable (Termy grid = native
default — no size key exists). noa driven via its now-working `-e` flag (verified:
`noa -e /bin/sh -c 'touch …'` succeeds).

## Full table (noa vs competitors)

| Axis (unit, better) | noa | Ghostty | Termy | kitty |
|---|---|---|---|---|
| Throughput ASCII (MiB/s ↑) | **146.9** | 73.1 | 145.5 | 104.5 |
| Throughput Unicode (MiB/s ↑) | **159.6** | 79.4 | 150.6 | 115.9 |
| Scroll median / best (ms ↓) | **217 / 207** | 971/945 | 272/265 | 370/367 |
| Latency median (µs ↓) | **18** | 31 | 16673 | 3846 |
| Latency p99 (µs ↓) | 70 | 68 | 18724 | 4964 |
| Startup **pty-ready** (ms ↓) | **62** | 189 | 179 | 248 |
| Startup **window-visible** (ms ↓) | 228 | **37** | 210 | 281 |
| Idle CPU avg / max (% ↓) | 0.44 / 1.1 | 1.42 / 7.2 | 0.44 / 0.9 | — |

Latency: noa/Ghostty are no-activation (they answer DSR off the render thread,
focus-independent); Termy/kitty *require* window activation to answer at all.
p99 from robust 1000-iteration runs (noa p99 ranged 60–81, Ghostty 67–70).

## noa delta across all runs

| Axis | baseline 010934 | post-merge 023318 | **v3 final** | net |
|---|---|---|---|---|
| Throughput ASCII | 126.2 | 138.5 | **146.9** | +16.4% ✅ now > Termy |
| Throughput Unicode | 118.1 | 148.6 | **159.6** | +35.1% ✅ now > Termy |
| Scroll best (ms) | 184 | 292 (regressed) | **207** | recovered, #1 |
| Latency median (µs) | 18 | 22 | **18** | back to best |
| Latency p99 (µs) | 54 | 74 | **70** | tail fix → ~tied Ghostty |
| Startup pty-ready (ms) | 254 | 91 | **62** | −76% ✅ |
| Startup window-visible | — | — | **228** | NEW metric (see below) |

## Rubric verdict — does noa beat BOTH Ghostty AND Termy?

1. **Throughput ASCII** — ✅ YES. 146.9 > Termy 145.5 (+1.0%) and Ghostty 73.1.
   Narrow over Termy but a genuine pass (was −3.4% at post-merge).
2. **Throughput Unicode** — ✅ YES. 159.6 > Termy 150.6 (+6.0%), Ghostty 79.4.
3. **Frame/Scroll** — ✅ YES. best 207 ms (median 217) < Termy 265/272 and
   Ghostty 945/971. Regression fixed; noa back to #1.
4. **Latency median** — ✅ YES. 18 µs < Ghostty 31, Termy 16673.
   **Latency p99** — ⚖️ TIE vs Ghostty (70 vs 68 µs, within run-to-run noise
   60–81 vs 67–70); ✅ beats Termy. Fix-3 closed the post-merge p99 gap (74→70);
   not a decisive win over Ghostty, but no longer a loss.
5. **Startup pty-ready** — ✅ YES. 62 ms < Ghostty 189, Termy 179, kitty 248.
   **Startup window-visible** — ❌ **NO.** 228 ms LOSES to Ghostty 37 ms and
   Termy 210 ms (beats only kitty 281). noa prespawns the pty child (hence the
   fastest pty-ready) but does not put pixels on screen until ~228 ms — it paints
   the window *last*, after the others already show theirs.

### Summary
- **noa beats BOTH competitors on:** throughput ASCII, throughput Unicode,
  scroll, latency-median, startup-**pty-ready**.
- **Ties:** latency p99 vs Ghostty.
- **noa loses on:** startup **window-visible** (228 ms; 3rd of 4). The "fast
  startup" claim is sound only when scoped to **pty-ready**; on the
  photon-honest window-on-screen metric noa is behind Ghostty and Termy.

## New-measurement results

**Startup window-visible parity** (CGWindowList polling @5 ms, no
screen-recording permission needed — reads window owner + bounds only; measured
identically for all four). Both sentinels reported above. The pty-ready vs
window-visible split is opposite for noa (62→228) and Ghostty (189→37): noa boots
the shell before the window, Ghostty shows the window before the shell. For a
user, "startup" ≈ window-visible, so this is the metric to headline — and it is
noa's one clear miss.

**Scroll retention-equalized** (Termy retains 1000 lines; noa default 10 MB):
- noa default 10 MB: best **207 ms** (193 MiB/s)
- noa ≈1000 lines (`scrollback-limit = 1000000`, ~1040 rows): best **188 ms**
  (213 MiB/s)
- Retention-work asymmetry ≈ **9.2%**. Even retention-matched, noa (188 ms) still
  beats Termy (265 ms) decisively — the scroll win is not an artifact of Termy
  throwing away history.

**Idle CPU (30 s, window open at shell prompt, no input):**
- noa avg **0.44%**, max 1.1% (one sampling blip) — **no regression**; the
  traffic-gated spin loops do not burn idle CPU. Ties Termy (0.44% / 0.9%) and
  idles cleaner than Ghostty (1.42% / 7.2% spike). Not flagged.

## Notes / bugs
- `-e` first-window exec is **FIXED** and used for noa throughout this run.
- Latency p99 is measurement-noisy at n=300; the 1000-iter runs are the
  authoritative p99 basis.
