# Terminal IO Throughput Benchmark

This project is a benchmark environment for measuring I/O throughput and rendering performance across various terminal emulators (Ghostty, Alacritty, Kitty, Warp, iTerm2, etc.).

## About the benchmark data
- `150MB_ascii.txt`: a 150MB text file consisting solely of ASCII characters.
- `150MB_unicode.txt`: a 150MB text file mixing Japanese, English, Korean, Chinese, Russian, emoji, and CSI (control sequence) characters.

## Quick start

### 1. Generate benchmark data
Use Python 3 to generate the two 150MB test text files.

```bash
python3 generate_data.py
```

### 2. Run the benchmark
To measure the terminal's rendering speed, the output is streamed directly to the terminal without redirection.

```bash
chmod +x run_benchmark.sh
./run_benchmark.sh
```

To measure manually and individually, run the following:
```bash
time cat 150MB_ascii.txt
time cat 150MB_unicode.txt
```
