# Terminal Benchmark — 20260716-010550

Terminals: noa Noa 0.1.4, ghostty 1.3.1, termy 0.2.21, kitty 0.47.4

Machine: Apple M4 (10 cores), macOS 26.5.1 (arm64)


## Throughput — ASCII (MiB/s, higher better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| ascii MiB/s | 70.0 | 106.9 | 124.4 | 145.9 |
| unicode MiB/s | 76.9 | 114.9 | 115.3 | 150.9 |

## Frame / Scroll (time ms / MiB·s⁻¹, lower ms better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| scroll_stress | 1027 ms / 39.0 | 368 ms / 108.7 | 179 ms / 223.0 | 276 ms / 144.7 |

## Input Latency — DSR round-trip proxy (median / p99 µs, lower better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| DSR µs | 25.0 / 56.0 | 3433.0 / 10510.0 | 17.0 / 89.0 | UNMEASURED |

## Warm Startup — spawn→pty-ready (ms, lower better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| startup ms | 165 | 275 | 284 | 177 |

## noa rank per axis
- throughput ascii: #2 of 4
- throughput unicode: #2 of 4
- scroll (MiB/s): #1 of 4
- latency (median): #1 of 3
- startup: #4 of 4
