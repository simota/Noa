# Terminal Benchmark — 20260716-034257

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
| ascii MiB/s | 73.1 | 104.5 | 146.9 | 145.5 |
| unicode MiB/s | 79.4 | 115.9 | 159.6 | 150.6 |

## Frame / Scroll (time ms / MiB·s⁻¹, lower ms better)
| Metric | ghostty | kitty | noa | termy |
|---|---|---|---|---|
| scroll_stress | 971 ms / 41.2 | 370 ms / 108.2 | 217 ms / 184.1 | 272 ms / 147.2 |

## noa rank per axis
- throughput ascii: #1 of 4
- throughput unicode: #1 of 4
- scroll (MiB/s): #1 of 4
