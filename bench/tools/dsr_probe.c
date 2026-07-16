// dsr_probe — input-latency proxy via DSR (Device Status Report) round-trips.
//
// Runs as the pty child inside a terminal under test. stdin/stdout are the
// pty slave, i.e. the terminal itself. We put the tty in raw mode, then N
// times: write ESC[6n (DSR cursor-position request), read the terminal's
// ESC[<row>;<col>R reply, and record the elapsed CLOCK_MONOTONIC nanoseconds.
//
// This measures the pty -> VT parser -> responder -> pty loop latency, NOT
// photon/keyboard-to-glass latency. It is a fair, fully-automatable proxy for
// how quickly a terminal's parser turns around a query, which is on the same
// code path that echoes keystrokes.
//
// Usage: dsr_probe <iterations> <warmup> <out_file> [samples_file]
//   Writes "<median_ns> <p95_ns> <p99_ns> <max_ns> <min_ns> <count>" to
//   out_file. If samples_file is given, additionally writes every kept
//   sample (one ns value per line, unsorted arrival order) so the harness
//   can pool raw samples across multiple process launches and compute
//   percentiles over the pooled distribution instead of medianing per-run
//   percentiles.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <termios.h>
#include <sys/select.h>

static long long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int cmp_ll(const void *a, const void *b) {
    long long x = *(const long long *)a, y = *(const long long *)b;
    return (x > y) - (x < y);
}

// Read one DSR reply: bytes ending in 'R'. Returns 0 on success, -1 on error
// or timeout. A per-reply select() timeout keeps a terminal that stalls its
// query responses (e.g. render-thread-throttled while unfocused) from hanging
// the whole probe — we abort and report whatever samples we already have.
static int read_reply(int timeout_ms) {
    char c;
    while (1) {
        fd_set rfds;
        FD_ZERO(&rfds);
        FD_SET(STDIN_FILENO, &rfds);
        struct timeval tv = { timeout_ms / 1000, (timeout_ms % 1000) * 1000 };
        int rv = select(STDIN_FILENO + 1, &rfds, NULL, NULL, &tv);
        if (rv <= 0) return -1;  // timeout or error
        ssize_t n = read(STDIN_FILENO, &c, 1);
        if (n <= 0) return -1;
        if (c == 'R') return 0;
    }
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: dsr_probe <iterations> <warmup> <out_file> [samples_file]\n");
        return 2;
    }
    int iters = atoi(argv[1]);
    int warmup = atoi(argv[2]);
    const char *out = argv[3];
    const char *samples_out = (argc >= 5 && argv[4][0] != '\0') ? argv[4] : NULL;
    if (iters <= 0) iters = 200;

    struct termios orig, raw;
    if (tcgetattr(STDIN_FILENO, &orig) != 0) {
        fprintf(stderr, "dsr_probe: stdin is not a tty\n");
        return 3;
    }
    raw = orig;
    cfmakeraw(&raw);
    raw.c_cc[VMIN] = 1;
    raw.c_cc[VTIME] = 0;
    tcsetattr(STDIN_FILENO, TCSANOW, &raw);

    int total = iters + warmup;
    long long *samples = calloc(iters, sizeof(long long));
    int kept = 0;

    for (int i = 0; i < total; i++) {
        long long t0 = now_ns();
        if (write(STDOUT_FILENO, "\x1b[6n", 4) != 4) break;
        if (read_reply(3000) != 0) break;
        long long dt = now_ns() - t0;
        if (i >= warmup && kept < iters) samples[kept++] = dt;
    }

    tcsetattr(STDIN_FILENO, TCSANOW, &orig);

    // Raw kept samples (arrival order) for cross-launch pooling.
    if (samples_out && kept > 0) {
        FILE *sf = fopen(samples_out, "w");
        if (sf) {
            for (int i = 0; i < kept; i++) fprintf(sf, "%lld\n", samples[i]);
            fclose(sf);
        }
    }

    FILE *f = fopen(out, "w");
    if (!f) { free(samples); return 4; }
    if (kept == 0) {
        fprintf(f, "0 0 0 0 0 0\n");
    } else {
        qsort(samples, kept, sizeof(long long), cmp_ll);
        long long med = samples[kept / 2];
        int p95i = (int)((double)kept * 0.95);
        if (p95i >= kept) p95i = kept - 1;
        int p99i = (int)((double)kept * 0.99);
        if (p99i >= kept) p99i = kept - 1;
        long long p95 = samples[p95i];
        long long p99 = samples[p99i];
        long long mx = samples[kept - 1];
        long long mn = samples[0];
        fprintf(f, "%lld %lld %lld %lld %lld %d\n", med, p95, p99, mx, mn, kept);
    }
    fclose(f);
    free(samples);
    return 0;
}
