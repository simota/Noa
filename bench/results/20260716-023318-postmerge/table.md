# Terminal Benchmark — 20260716-023318-postmerge

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
| ascii MiB/s | 70.7 | 103.6 | 138.5 | 143.4 |
| unicode MiB/s | 78.3 | 114.7 | 148.6 | 150.1 |

## Frame / Scroll (time ms / MiB·s⁻¹, lower ms better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| scroll_stress | 973 ms / 41.1 | 380 ms / 105.4 | 328 ms / 122.1 | 273 ms / 146.8 |

## Input Latency — DSR round-trip proxy (median / p99 µs, lower better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| DSR µs | 38.5 / 65.0 | 3814.0 / 4083.5 | 29.0 / 167.5 | 16664.5 / 36957.5 |

## Warm Startup — spawn→pty-ready (ms, lower better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| startup ms | 173 | 289 | 91 | 199 |

## noa rank per axis
- throughput ascii: #2 of 4
- throughput unicode: #2 of 4
- scroll (MiB/s): #2 of 4
- latency (median): #1 of 4
- startup: #1 of 4
