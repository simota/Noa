# Terminal Benchmark — 20260716-044333

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
| ascii MiB/s | 72.7 | 104.7 | 153.5 | 140.7 |
| unicode MiB/s | 81.0 | 115.8 | 162.5 | 151.1 |

## Frame / Scroll (time ms / MiB·s⁻¹, lower ms better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| scroll_stress | 967 ms / 41.4 | 382 ms / 104.7 | 242 ms / 165.1 | 275 ms / 145.4 |

## noa rank per axis
- throughput ascii: #1 of 4
- throughput unicode: #1 of 4
- scroll (MiB/s): #1 of 4
