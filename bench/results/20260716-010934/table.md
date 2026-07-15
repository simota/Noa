# Terminal Benchmark — 20260716-010934

Terminals: noa Noa 0.1.4, ghostty 1.3.1, termy 0.2.21, kitty 0.47.4

Machine: Apple M4 (10 cores), macOS 26.5.1 (arm64)


## Throughput — ASCII (MiB/s, higher better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| ascii MiB/s | 68.2 | 107.6 | 126.2 | 131.9 |
| unicode MiB/s | 77.7 | 115.3 | 118.1 | 150.4 |

## Frame / Scroll (time ms / MiB·s⁻¹, lower ms better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| scroll_stress | 1042 ms / 38.4 | 367 ms / 109.1 | 184 ms / 217.9 | 275 ms / 145.6 |

## Input Latency — DSR round-trip proxy (median / p99 µs, lower better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| DSR µs | 35.5 / 66.5 | 3451.5 / 4114.0 | 18.0 / 54.0 | UNMEASURED |

## Warm Startup — spawn→pty-ready (ms, lower better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| startup ms | 168 | 276 | 254 | 191 |

## noa rank per axis
- throughput ascii: #2 of 4
- throughput unicode: #2 of 4
- scroll (MiB/s): #1 of 4
- latency (median): #1 of 3
- startup: #3 of 4
