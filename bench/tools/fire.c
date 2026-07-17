// fire.c — DOOM-fire IO stress workload (bench axis "fire").
//
// Renders the classic DOOM fire effect (Fabien Sanglard's algorithm, the
// workload popularized as a terminal benchmark by DOOM-fire-zig) into a
// FIXED 80x24 cell region as truecolor half-blocks: every frame repaints the
// whole region with per-cell SGR 38;2/48;2 RGB + U+2584. The region is fixed
// (not window-sized) so every terminal consumes a byte-identical stream —
// fps is inversely proportional to cell count and Termy's grid size is not
// pinnable (see docs/specs/bench-doom-fire.md, decision 2).
//
// Producer-side fps under pty flow control: the pty's small kernel buffer
// makes write() block until the terminal drains, so frames-written/second
// ~= frames-consumed/second — the same "consume the pipe" proxy as the
// throughput axis. Frame size is constant (no SGR run-length dedupe), so
// fps maps linearly to drain MiB/s.
//
// Usage: fire <seconds> <result-file> [full]
//   60 warmup frames (discarded: atlas population, alt-screen entry), then
//   render flat-out for <seconds>, then write
//   "<frames> <elapsed_ns> <fps> <winsize-cols>x<rows> <region-cols>x<rows>"
//   to <result-file>.
//
//   Default: fixed 80x24 region (the SCORED harness condition — byte-
//   identical stream for every terminal). `full` renders to the live window
//   size instead, approximating upstream DOOM-fire-zig's full-window
//   condition for manual anchor runs; fps scales ~1/cell-count, so full-mode
//   numbers are window-geometry-dependent and never enter the scored axis.
//
// Deterministic: fixed xorshift32 seed -> identical frame sequence every
// run (verify: two runs redirected to files agree on their common prefix;
// total length differs only by how many frames fit in the wall-clock
// budget).

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

#define DEF_W 80         /* default region: cells wide */
#define DEF_ROWS 24      /* default region: cell rows */
#define MAX_W 1000       /* full-mode safety caps for buffer sizing */
#define MAX_ROWS 500
#define NPAL 37
#define WARMUP_FRAMES 60

/* Region geometry — fixed 80x24 by default, live winsize in `full` mode. */
static int W = DEF_W;        /* cells wide = fire pixels wide */
static int ROWS = DEF_ROWS;  /* cell rows */
static int PH = DEF_ROWS * 2; /* fire pixels tall (2 per cell row) */

/* Sanglard's 37-entry DOOM fire palette (black -> red -> yellow -> white). */
static const uint8_t PAL[NPAL][3] = {
    {0x07, 0x07, 0x07}, {0x1F, 0x07, 0x07}, {0x2F, 0x0F, 0x07},
    {0x47, 0x0F, 0x07}, {0x57, 0x17, 0x07}, {0x67, 0x1F, 0x07},
    {0x77, 0x1F, 0x07}, {0x8F, 0x27, 0x07}, {0x9F, 0x2F, 0x07},
    {0xAF, 0x3F, 0x07}, {0xBF, 0x47, 0x07}, {0xC7, 0x47, 0x07},
    {0xDF, 0x4F, 0x07}, {0xDF, 0x57, 0x07}, {0xDF, 0x57, 0x07},
    {0xD7, 0x5F, 0x07}, {0xD7, 0x5F, 0x07}, {0xD7, 0x67, 0x0F},
    {0xCF, 0x6F, 0x0F}, {0xCF, 0x77, 0x0F}, {0xCF, 0x7F, 0x0F},
    {0xCF, 0x87, 0x17}, {0xC7, 0x87, 0x17}, {0xC7, 0x8F, 0x17},
    {0xC7, 0x97, 0x1F}, {0xBF, 0x9F, 0x1F}, {0xBF, 0x9F, 0x1F},
    {0xBF, 0xA7, 0x27}, {0xBF, 0xA7, 0x27}, {0xBF, 0xAF, 0x2F},
    {0xB7, 0xAF, 0x2F}, {0xB7, 0xB7, 0x2F}, {0xB7, 0xB7, 0x37},
    {0xCF, 0xCF, 0x6F}, {0xDF, 0xDF, 0x9F}, {0xEF, 0xEF, 0xC7},
    {0xFF, 0xFF, 0xFF},
};

/* Precomposed SGR strings per palette index (frame composition must stay far
 * faster than the pty drain rate; per-cell sprintf would cap the producer). */
static char sgr_bg[NPAL][24], sgr_fg[NPAL][24];
static int sgr_bg_len[NPAL], sgr_fg_len[NPAL];

static uint8_t *fire;
static char *frame;

static uint32_t rng = 0x9d2c5680u; /* fixed seed: deterministic stream */
static uint32_t xorshift32(void) {
    rng ^= rng << 13;
    rng ^= rng >> 17;
    rng ^= rng << 5;
    return rng;
}

/* One simulation step: propagate heat upward with horizontal drift+decay. */
static void spread(void) {
    for (int x = 0; x < W; x++) {
        for (int y = 1; y < PH; y++) {
            int src = y * W + x;
            uint8_t p = fire[src];
            if (p == 0) {
                fire[src - W] = 0;
            } else {
                int r = (int)(xorshift32() & 3);
                int dst = src - r + 1;
                if (dst < W) dst = W; /* keep the target row in-buffer */
                fire[dst - W] = (uint8_t)(p - (r & 1));
            }
        }
    }
}

/* Compose one full-region repaint: per cell row an absolute CUP, then per
 * cell bg=upper pixel, fg=lower pixel, U+2584 lower half block. */
static size_t render(void) {
    char *p = frame;
    for (int row = 0; row < ROWS; row++) {
        p += sprintf(p, "\x1b[%d;1H", row + 1);
        const uint8_t *up = &fire[(row * 2) * W];
        const uint8_t *lo = &fire[(row * 2 + 1) * W];
        for (int x = 0; x < W; x++) {
            memcpy(p, sgr_bg[up[x]], (size_t)sgr_bg_len[up[x]]);
            p += sgr_bg_len[up[x]];
            memcpy(p, sgr_fg[lo[x]], (size_t)sgr_fg_len[lo[x]]);
            p += sgr_fg_len[lo[x]];
            memcpy(p, "\xe2\x96\x84", 3);
            p += 3;
        }
    }
    memcpy(p, "\x1b[0m", 4); /* reset so attributes don't bleed past the region */
    p += 4;
    return (size_t)(p - frame);
}

static int write_all(const void *buf, size_t len) {
    const char *p = buf;
    while (len > 0) {
        ssize_t w = write(STDOUT_FILENO, p, len);
        if (w < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        p += w;
        len -= (size_t)w;
    }
    return 0;
}

static long long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

int main(int argc, char **argv) {
    int full = (argc == 4 && strcmp(argv[3], "full") == 0);
    if (argc != 3 && !full) {
        fprintf(stderr, "usage: %s <seconds> <result-file> [full]\n", argv[0]);
        return 64;
    }
    double secs = atof(argv[1]);
    if (secs <= 0) {
        fprintf(stderr, "fire: bad duration '%s'\n", argv[1]);
        return 64;
    }

    struct winsize ws = {0};
    ioctl(STDOUT_FILENO, TIOCGWINSZ, &ws); /* audit; region source in full mode */
    if (full && ws.ws_col >= 2 && ws.ws_row >= 2) {
        W = ws.ws_col;
        ROWS = ws.ws_row;
        if (W > MAX_W) W = MAX_W;
        if (ROWS > MAX_ROWS) ROWS = MAX_ROWS;
        PH = ROWS * 2;
    }
    fire = calloc((size_t)(PH * W), 1);
    /* per cell worst case: two 19-byte SGRs + 3-byte glyph = 41; +16/row CUP */
    frame = malloc((size_t)ROWS * ((size_t)W * 41 + 16) + 64);
    if (!fire || !frame) return 1;

    for (int i = 0; i < NPAL; i++) {
        sgr_bg_len[i] = sprintf(sgr_bg[i], "\x1b[48;2;%d;%d;%dm",
                                PAL[i][0], PAL[i][1], PAL[i][2]);
        sgr_fg_len[i] = sprintf(sgr_fg[i], "\x1b[38;2;%d;%d;%dm",
                                PAL[i][0], PAL[i][1], PAL[i][2]);
    }
    for (int x = 0; x < W; x++) fire[(PH - 1) * W + x] = NPAL - 1;

    /* alt screen + hidden cursor + clear; restored on exit */
    if (write_all("\x1b[?1049h\x1b[?25l\x1b[2J", 18) < 0) return 1;

    for (int i = 0; i < WARMUP_FRAMES; i++) {
        spread();
        if (write_all(frame, render()) < 0) return 1;
    }

    long frames = 0;
    long long t0 = now_ns(), t1 = t0;
    long long deadline = t0 + (long long)(secs * 1e9);
    for (;;) {
        spread();
        if (write_all(frame, render()) < 0) return 1;
        frames++;
        t1 = now_ns();
        if (t1 >= deadline) break;
    }

    write_all("\x1b[0m\x1b[?25h\x1b[?1049l", 18);

    double fps = (double)frames / ((double)(t1 - t0) / 1e9);
    FILE *f = fopen(argv[2], "w");
    if (!f) return 1;
    fprintf(f, "%ld %lld %.1f %dx%d %dx%d\n", frames, t1 - t0, fps,
            (int)ws.ws_col, (int)ws.ws_row, W, ROWS);
    fclose(f);
    return 0;
}
