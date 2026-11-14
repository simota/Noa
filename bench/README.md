# Terminal IO Throughput Benchmark

A benchmark environment for measuring I/O throughput and rendering performance across terminal emulators (Ghostty, Alacritty, Kitty, Warp, iTerm2, etc.).

This page covers the quick manual throughput check. The reproducible
multi-axis suite behind the numbers in `README.md` and `docs/positioning.md`
is `run_all.sh`; see [METHODOLOGY.md](METHODOLOGY.md).

## Quick start

### 1. Generate benchmark data
Use Python 3 to generate the two 150MB test text files.

```bash
python3 generate_data.py
```

- `150MB_ascii.txt`: ASCII characters only.
- `150MB_unicode.txt`: a mix of Japanese, English, Korean, Chinese, Russian, emoji, and ANSI color escape sequences (SGR).

### 2. Run the benchmark
To measure the terminal's rendering speed, the output is streamed directly to the terminal without redirection.

```bash
chmod +x run_benchmark.sh
./run_benchmark.sh
```

To measure each file by hand:
```bash
time cat 150MB_ascii.txt
time cat 150MB_unicode.txt
```
