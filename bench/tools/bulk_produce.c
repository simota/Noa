// bulk_produce — bounded-duration bulk pty writer for the
// "latency-under-load" bench axis (bench/METHODOLOGY.md, axis 8).
//
// Streams a file's bytes to stdout in a loop, under the SAME pty flow
// control as the throughput axis's `cat <file>` (write() blocks until the
// terminal drains its kernel buffer), until a wall-clock DURATION elapses —
// not until N bytes are sent. This is the "bounded-duration equivalent" of
// the 150MB `cat` workload: a single `cat` of 150MB can complete in well
// under a second on a fast terminal, too short a window to hold a
// concurrent DSR probe through a meaningful number of iterations, so this
// tool re-streams the file for as long as the caller needs contention held.
//
// Two outputs:
//   - result-file: "<total_bytes> <elapsed_ns> <t0_ns> <t1_ns>" written once,
//     at exit — <total_bytes>/<elapsed_ns> gives the harness sustained MiB/s
//     over the exact window this process was actually writing; <t0_ns>/
//     <t1_ns> (CLOCK_MONOTONIC, same clock nowns/dsr_probe use) let the
//     harness verify this process's own active interval CONTAINED another
//     process's interval (e.g. the concurrent DSR probe's), which is a
//     cadence-independent overlap proof — unlike counting progress-log
//     points, which can under-sample a probe window shorter than the
//     progress cadence.
//   - progress-file (optional): "<CLOCK_MONOTONIC ns> <cumulative_bytes>"
//     appended roughly every 200ms while writing. This is NOT used for the
//     throughput number (that comes from the exact start/end result-file
//     pair) — it exists so the harness can verify, after the fact, that the
//     producer was STILL ACTIVELY WRITING throughout the concurrent DSR
//     probe's own wall-clock window (see run_latency_under_load in
//     run_all.sh), rather than assuming overlap from the launch order alone.
//
// Usage: bulk_produce <seconds> <file> <result-file> [progress-file]
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static long long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int write_all(int fd, const char *buf, size_t len) {
    const char *p = buf;
    while (len > 0) {
        ssize_t w = write(fd, p, len);
        if (w < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        p += w;
        len -= (size_t)w;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr,
                "usage: %s <seconds> <file> <result-file> [progress-file]\n",
                argv[0]);
        return 64;
    }
    double secs = atof(argv[1]);
    const char *file = argv[2];
    const char *result_path = argv[3];
    const char *progress_path = (argc >= 5 && argv[4][0] != '\0') ? argv[4] : NULL;
    if (secs <= 0) {
        fprintf(stderr, "bulk_produce: bad duration '%s'\n", argv[1]);
        return 64;
    }

    FILE *fp = fopen(file, "rb");
    if (!fp) { perror("bulk_produce: fopen"); return 1; }
    if (fseek(fp, 0, SEEK_END) != 0) { fclose(fp); return 1; }
    long fsize = ftell(fp);
    if (fsize <= 0) { fclose(fp); fprintf(stderr, "bulk_produce: empty file\n"); return 1; }
    rewind(fp);
    char *buf = malloc((size_t)fsize);
    if (!buf) { fclose(fp); return 1; }
    if (fread(buf, 1, (size_t)fsize, fp) != (size_t)fsize) {
        fclose(fp); free(buf);
        fprintf(stderr, "bulk_produce: short read\n");
        return 1;
    }
    fclose(fp);

    FILE *pf = NULL;
    if (progress_path) {
        pf = fopen(progress_path, "w");
        if (pf) setvbuf(pf, NULL, _IOLBF, 0); // line-buffered: partial log survives a hard kill
    }

    long long t0 = now_ns();
    long long deadline = t0 + (long long)(secs * 1e9);
    long long total = 0;
    long long last_progress = t0;
    const long long progress_interval_ns = 200000000LL; // ~200ms cadence

    while (now_ns() < deadline) {
        if (write_all(STDOUT_FILENO, buf, (size_t)fsize) < 0) break; // pty/pipe gone
        total += fsize;
        long long t = now_ns();
        if (pf && (t - last_progress) >= progress_interval_ns) {
            fprintf(pf, "%lld %lld\n", t, total);
            last_progress = t;
        }
    }
    long long t1 = now_ns();
    if (pf) {
        fprintf(pf, "%lld %lld\n", t1, total); // final point, always recorded
        fclose(pf);
    }

    // "<bytes> <elapsed_ns> <t0_ns> <t1_ns>" — t0/t1 are the producer's own
    // measured CLOCK_MONOTONIC start/finish, the same clock nowns/dsr_probe
    // use in the same pty child, so a caller can directly check whether
    // this process's active interval CONTAINED another process's interval
    // (see run_all.sh: run_latency_under_load's overlap check) instead of
    // relying on the coarser progress-log cadence, which can be wider than
    // a short probe window and would otherwise false-negative real overlap.
    FILE *rf = fopen(result_path, "w");
    if (rf) {
        fprintf(rf, "%lld %lld %lld %lld\n", total, t1 - t0, t0, t1);
        fclose(rf);
    }
    free(buf);
    return 0;
}
