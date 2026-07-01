#!/usr/bin/env bash
# Generate assets/noa.icns from scratch — no external image tools required
# (pure-stdlib Python draws the master PNG; macOS `sips` + `iconutil` build the
# .icns). Safe to re-run; overwrites assets/noa.icns.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="$ROOT/assets"
mkdir -p "$OUT_DIR"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

MASTER="$WORK/noa-1024.png"

python3 - "$MASTER" <<'PY'
import sys, struct, zlib

OUT = sys.argv[1]
N = 1024          # final master size
SS = 2            # supersample factor for anti-aliasing
W = N * SS

BG  = (29, 31, 33)      # dark terminal background
LT  = (220, 220, 220)   # prompt chevron
GRN = (38, 162, 105)    # block cursor (terminal green)

# Rounded-rect (squircle-ish) background geometry, in supersampled pixels.
M = int(W * 0.085)          # margin
R = int(W * 0.225)          # corner radius
x0, y0, x1, y1 = M, M, W - M, W - M

def in_rrect(px, py):
    # inside the straight body?
    if x0 + R <= px <= x1 - R and y0 <= py <= y1:
        return True
    if x0 <= px <= x1 and y0 + R <= py <= y1 - R:
        return True
    # corners
    for cx, cy in ((x0 + R, y0 + R), (x1 - R, y0 + R), (x0 + R, y1 - R), (x1 - R, y1 - R)):
        if px < cx - R or px > cx + R or py < cy - R or py > cy + R:
            continue
        if (px - cx) ** 2 + (py - cy) ** 2 <= R * R:
            return True
    return False

def seg_dist(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    l2 = dx * dx + dy * dy
    t = 0.0 if l2 == 0 else max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / l2))
    cx, cy = ax + t * dx, ay + t * dy
    return ((px - cx) ** 2 + (py - cy) ** 2) ** 0.5

# Prompt chevron ">" (two thick strokes) + a block cursor to its right.
T = W * 0.052                        # chevron stroke half is T/2
cax, cay = W * 0.40, W * 0.40        # chevron top
cbx, cby = W * 0.545, W * 0.50       # chevron apex
ccx, ccy = W * 0.40, W * 0.60        # chevron bottom
# chevron bounding box (skip the expensive distance test elsewhere)
chx0, chy0, chx1, chy1 = W * 0.34, W * 0.34, W * 0.60, W * 0.66
# block cursor rectangle
bx0, by0, bx1, by1 = W * 0.60, W * 0.435, W * 0.72, W * 0.565

buf = bytearray(W * W * 4)
for py in range(W):
    row = py * W * 4
    for px in range(W):
        i = row + px * 4
        if not in_rrect(px, py):
            continue  # transparent (alpha stays 0)
        r, g, b = BG
        if bx0 <= px <= bx1 and by0 <= py <= by1:
            r, g, b = GRN
        elif chx0 <= px <= chx1 and chy0 <= py <= chy1:
            d = min(seg_dist(px, py, cax, cay, cbx, cby),
                    seg_dist(px, py, cbx, cby, ccx, ccy))
            if d <= T / 2:
                r, g, b = LT
        buf[i] = r; buf[i+1] = g; buf[i+2] = b; buf[i+3] = 255

# Box-downsample W -> N (SSxSS average) for anti-aliasing.
out = bytearray(N * N * 4)
for oy in range(N):
    for ox in range(N):
        ar = ag = ab = aa = 0
        for dy in range(SS):
            base = ((oy * SS + dy) * W + ox * SS) * 4
            for dx in range(SS):
                j = base + dx * 4
                ar += buf[j]; ag += buf[j+1]; ab += buf[j+2]; aa += buf[j+3]
        k = SS * SS
        o = (oy * N + ox) * 4
        out[o] = ar // k; out[o+1] = ag // k; out[o+2] = ab // k; out[o+3] = aa // k

def png(path, w, h, rgba):
    def chunk(typ, data):
        c = typ + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xffffffff)
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw += rgba[y*w*4:(y+1)*w*4]
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(bytes(raw), 9)))
        f.write(chunk(b"IEND", b""))

png(OUT, N, N, out)
print(f"wrote {OUT}")
PY

# Build the .iconset (all required sizes) from the master, then the .icns.
ICONSET="$WORK/noa.iconset"
mkdir -p "$ICONSET"
for sz in 16 32 128 256 512; do
  sips -z "$sz" "$sz"           "$MASTER" --out "$ICONSET/icon_${sz}x${sz}.png"    >/dev/null
  sips -z $((sz*2)) $((sz*2))   "$MASTER" --out "$ICONSET/icon_${sz}x${sz}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$OUT_DIR/noa.icns"
echo "Built $OUT_DIR/noa.icns"
