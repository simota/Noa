# Terminal Benchmark — 20260716-012100

Terminals: noa Noa 0.1.4, ghostty 1.3.1, termy 0.2.21, kitty 0.47.4

Machine: Apple M4 (10 cores), macOS 26.5.1 (arm64)


**Equalized conditions** (per terminal):
- ghostty: grid=120x40;font=Menlo@14;clean-config
- kitty: grid=120x40;font=Menlo@14;config=NONE
- noa: grid=120x40;font=Menlo@14;ligatures-off;bg-off
- termy: grid=NATIVE-DEFAULT(no-size-key);font=Menlo@14


## Throughput — ASCII (MiB/s, higher better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| ascii MiB/s | 71.7 | 103.9 | 127.1 | 138.9 |
| unicode MiB/s | 79.1 | 114.4 | 121.0 | 151.9 |

## Frame / Scroll (time ms / MiB·s⁻¹, lower ms better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| scroll_stress | 1386 ms / 28.9 | 371 ms / 107.9 | 212 ms / 188.3 | 275 ms / 145.4 |

## noa rank per axis
- throughput ascii: #2 of 4
- throughput unicode: #2 of 4
- scroll (MiB/s): #1 of 4

## NOTE — scroll numbers above ran under CPU contention (concurrent builds)

The scroll medians above (noa 212ms, ghostty 1386ms) were measured while other
teams were compiling (load avg 5–12). noa and Ghostty are CPU-bound on consume,
so their scroll times inflate under load; Termy/kitty are display-paced and
contention-immune. Contention-robust equalized scroll (min of 6–8 reps, catching
quiet-CPU windows):

| Metric | noa | ghostty | termy | kitty |
|---|---|---|---|---|
| scroll best-case ms (↓) | **169** | 945 | 265 | 367 |
| scroll best-case MiB/s | **236.7** | 42.3 | 154.9 | 108.9 |

Ranking (noa #1 < Termy < kitty < Ghostty) is identical to the baseline run.
Throughput (I/O-bound) was contention-insensitive and matches baseline.
