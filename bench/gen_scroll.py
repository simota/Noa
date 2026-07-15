#!/usr/bin/env python3
"""Generate scroll_stress.txt — a render-pressure workload.

Unlike the plain 150MB throughput files, this stresses the *render* path:
every line repaints with fresh SGR color state, and every few hundred lines a
DECSTBM scrolling region is set/reset so the terminal exercises its scroll-
region fast/slow paths instead of a single flat scroll. Consuming it end to
end is the frame/scroll proxy.
"""
import os
import sys

TARGET_MB = int(sys.argv[1]) if len(sys.argv) > 1 else 40
OUT = sys.argv[2] if len(sys.argv) > 2 else "scroll_stress.txt"

ESC = "\x1b"
target = TARGET_MB * 1024 * 1024

# A palette of SGR openers cycling fg/bg 256-color + attrs, so each line forces
# an attribute transition rather than reusing cached run state.
palette = [
    f"{ESC}[38;5;{fg};48;5;{bg};{attr}m"
    for fg in (196, 46, 51, 226, 201, 129)
    for bg in (17, 52, 22)
    for attr in (1, 3, 4, 7)
]
RESET = f"{ESC}[0m"

payload = "scrolling region SGR churn 0123456789 ABCDEFGHIJ ~!@#$%^&*() "

written = 0
line_no = 0
buf = []
buf_bytes = 0
with open(OUT, "w", encoding="utf-8") as f:
    while written < target:
        # Periodically toggle a scrolling region and home the cursor so the
        # terminal has to handle DECSTBM + relative scrolls, not just append.
        if line_no % 500 == 0:
            region = f"{ESC}[3;24r{ESC}[H"
            buf.append(region)
            buf_bytes += len(region)
        color = palette[line_no % len(palette)]
        line = f"{color}{payload}{payload}{RESET}\n"
        buf.append(line)
        b = len(line.encode("utf-8"))
        buf_bytes += b
        written += b
        line_no += 1
        if buf_bytes >= 1024 * 1024:
            f.write("".join(buf))
            buf = []
            buf_bytes = 0
    # Reset scroll region at the very end so we leave the terminal sane.
    buf.append(f"{ESC}[r")
    f.write("".join(buf))

print(f"Generated {OUT} ({os.path.getsize(OUT) / (1024*1024):.2f} MB, {line_no} lines)")
