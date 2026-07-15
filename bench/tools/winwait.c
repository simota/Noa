// winwait — poll the window server until a visible, on-screen window owned by a
// named process appears; print elapsed milliseconds since program start.
//
// Used for the startup "window actually on screen" sentinel, measured
// identically for every terminal. Listing window owner names + bounds via
// CGWindowListCopyWindowInfo needs NO screen-recording permission (only reading
// window *contents* does), so this is portable across terminals.
//
// Usage: winwait <owner-substring> <timeout_ms>
//   Exit 0 and print "<ms>" once a layer-0 window with non-empty bounds whose
//   owner name contains <owner-substring> (case-insensitive) is on screen.
//   Exit 1 and print "TIMEOUT" otherwise.
#include <ApplicationServices/ApplicationServices.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <time.h>
#include <unistd.h>

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

static int matches(const char *owner, const char *needle) {
    return owner && strcasestr(owner, needle) != NULL;
}

int main(int argc, char **argv) {
    if (argc < 3) { fprintf(stderr, "usage: winwait <owner> <timeout_ms>\n"); return 2; }
    const char *needle = argv[1];
    long timeout_ms = atol(argv[2]);
    double start = now_ms();

    while (now_ms() - start < timeout_ms) {
        CFArrayRef list = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID);
        if (list) {
            CFIndex n = CFArrayGetCount(list);
            for (CFIndex i = 0; i < n; i++) {
                CFDictionaryRef w = CFArrayGetValueAtIndex(list, i);
                // layer 0 == normal app window (skip menubars/overlays)
                int layer = 1;
                CFNumberRef ln = CFDictionaryGetValue(w, kCGWindowLayer);
                if (ln) CFNumberGetValue(ln, kCFNumberIntType, &layer);
                if (layer != 0) continue;
                // owner name
                CFStringRef on = CFDictionaryGetValue(w, kCGWindowOwnerName);
                if (!on) continue;
                char owner[256];
                if (!CFStringGetCString(on, owner, sizeof owner, kCFStringEncodingUTF8)) continue;
                if (!matches(owner, needle)) continue;
                // non-empty bounds
                CFDictionaryRef b = CFDictionaryGetValue(w, kCGWindowBounds);
                if (!b) continue;
                CGRect r;
                if (!CGRectMakeWithDictionaryRepresentation(b, &r)) continue;
                if (r.size.width < 20 || r.size.height < 20) continue;
                CFRelease(list);
                printf("%.1f\n", now_ms() - start);
                return 0;
            }
            CFRelease(list);
        }
        usleep(5000);  // 5ms
    }
    printf("TIMEOUT\n");
    return 1;
}
