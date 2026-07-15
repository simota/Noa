// nowns — print a monotonic timestamp in integer nanoseconds.
// Used by wrapper.sh to bracket workloads with low-overhead timestamps
// (a shelled-out python/perl call would add tens of ms of noise).
#include <stdio.h>
#include <time.h>

int main(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    printf("%lld\n", (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec);
    return 0;
}
