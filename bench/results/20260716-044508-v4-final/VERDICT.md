# FINAL decisive run (v4) — `perf/fastest-terminal` HEAD c3fcd93

Apple M4 (10 cores), macOS 26.5.1, arm64. Fresh release build (cycle-3
window-visible fix merged, tests green). Quiet machine (load 2–3.4 throughout).
Equalized 120×40 / Menlo 14 where pinnable (Termy grid = native default — no size
key). noa driven via its working `-e` flag. **This is the number set for the
Fulfillment Report.**

## Full head-to-head table

| Axis (unit, better) | noa | Ghostty | Termy | kitty |
|---|---|---|---|---|
| Throughput ASCII (MiB/s ↑) | **153.5** | 72.7 | 140.7 | 104.7 |
| Throughput Unicode (MiB/s ↑) | **162.5** | 81.0 | 151.1 | 115.8 |
| Scroll median / best (ms ↓) | **242 / 192** | 967/945 | 275/275 | 382/382 |
| Latency median (µs ↓) | **16** | 36 | 16671 | 3844 |
| Latency p99 (µs ↓) | **51** | 151 | 20420 | 4604 |
| Startup pty-ready (ms ↓) | **66** | 198 | 183 | 257 |
| Startup window-visible (ms ↓) | 143 | **42** | 211 | 295 |
| Startup usable = max(both) (ms ↓) | **143** | 198 | 211 | 295 |
| Idle CPU avg / max (% ↓) | 0.57 / 1.3 | — | — | — |

Latency: noa/Ghostty are no-activation, **interleaved 2000-iteration ×3** (fair
pairs). Per-round p99 — noa 42/51/53 µs, Ghostty 153/129/151 µs: noa's tail is
consistently ~3× tighter. Termy/kitty require window activation to respond at all.

## Progression across all runs (noa)

| Axis | baseline | post-merge | v3 | **v4 FINAL** |
|---|---|---|---|---|
| Throughput ASCII (MiB/s) | 126.2 | 138.5 | 146.9 | **153.5** |
| Throughput Unicode (MiB/s) | 118.1 | 148.6 | 159.6 | **162.5** |
| Scroll best (ms) | 184 | 292 | 207 | **192** |
| Latency median (µs) | 18 | 22 | 18 | **16** |
| Latency p99 (µs) | 54 | 74 | 70 | **51** |
| Startup pty-ready (ms) | 254 | 91 | 62 | **66** |
| Startup window-visible (ms) | — | — | 228 | **143** |
| Startup usable (ms) | — | — | 228 | **143** |

Every axis is at or near its best-ever in v4. Window-visible went 228→143 ms
(cycle-3 pre-painted-frame fix); the p99 tail 74→51 µs.

## FINAL RUBRIC VERDICT — does noa beat BOTH Ghostty AND Termy?

| # | Sub-metric | noa | Ghostty | Termy | Beats BOTH? |
|---|---|---|---|---|---|
| 1 | Throughput ASCII | 153.5 | 72.7 | 140.7 | ✅ **YES** |
| 2 | Throughput Unicode | 162.5 | 81.0 | 151.1 | ✅ **YES** |
| 3 | Scroll (best ms) | 192 | 945 | 275 | ✅ **YES** |
| 4 | Latency median (µs) | 16 | 36 | 16671 | ✅ **YES** |
| 5 | Latency p99 (µs) | 51 | 151 | 20420 | ✅ **YES** |
| 6 | Startup pty-ready (ms) | 66 | 198 | 183 | ✅ **YES** |
| 7 | Startup window-visible (ms) | 143 | 42 | 211 | ❌ **NO** (beats Termy, loses Ghostty 42) |
| 8 | Startup usable = max (ms) | 143 | 198 | 211 | ✅ **YES** |

### Score: noa beats BOTH competitors on 7 of 8 sub-metrics.
The sole miss is **startup window-visible** (143 ms vs Ghostty's 42 ms): Ghostty
paints its window frame almost instantly (then boots the shell for 198 ms),
whereas noa reaches pty-ready first (66 ms) and paints at 143 ms. Because
Ghostty's pty is slow, on **usable = max(pty-ready, window-visible)** — the point
where the terminal both shows AND can run a command — **noa wins (143 vs 198 ms)**.
So the only metric noa trails on is "empty frame on screen", not "ready to use".

## New / refined measurements

**Both startup sentinels + usable** (CGWindowList @5 ms, no screen-recording
permission). Reported in the table. noa: pty 66 / window 143 / usable 143.

**Latency 2000-iter ×3 interleaved** — the p99-vs-Ghostty race is now settled,
not a tie: the larger, interleaved samples show Ghostty's p99 is ~150 µs while
noa's is ~51 µs. noa wins median AND p99 decisively.

**Scroll retention-equalized (best-of-8):** default 10 MB = 192 ms, limited to
≈1000 lines (`scrollback-limit=1000000`) = 195 ms. **Asymmetry ≈ 0** (within
noise) — post-fix, noa's scroll no longer pays a retention tax, so its scroll win
over Termy (192 vs 275) is not an artifact of Termy discarding history.

**Idle CPU (noa, 30 s):** avg 0.57 %, max 1.3 %. Valleys at 0.0–0.1 % prove
there is **no busy-spin** — the prewarm/startup workers park after boot as
intended. The occasional 1.2–1.3 % blips are periodic UI timers (cursor blink),
not a spin loop. No regression.

## Notes
- `-e` first-window exec: FIXED, verified (`noa -e /bin/sh -c 'touch …'`), used
  to drive noa throughout.
- All numbers median-of-N (throughput 3, scroll best-of-8, latency 2000×3,
  startup 5); machine quiet the whole run.
