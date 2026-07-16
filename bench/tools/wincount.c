// wincount — count visible, on-screen, layer-0 windows owned by the given
// pids; print the count.
//
// PID-scoped companion to winwait: the multitab fairness check must verify
// that every terminal actually materialized the requested number of windows,
// and matching by owner NAME (like winwait does for startup) would risk
// counting the user's own concurrently-running instance of the same terminal.
// Like winwait, CGWindowListCopyWindowInfo needs no screen-recording
// permission for owner/bounds metadata.
//
// Usage: wincount <pid> [pid...]   -> prints "<count>\n", exit 0
#include <ApplicationServices/ApplicationServices.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc < 2) { printf("0\n"); return 0; }

    CFArrayRef list = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID);
    int count = 0;
    if (list) {
        CFIndex n = CFArrayGetCount(list);
        for (CFIndex i = 0; i < n; i++) {
            CFDictionaryRef w = CFArrayGetValueAtIndex(list, i);
            // layer 0 == normal app window (skip menubars/overlays)
            int layer = 1;
            CFNumberRef ln = CFDictionaryGetValue(w, kCGWindowLayer);
            if (ln) CFNumberGetValue(ln, kCFNumberIntType, &layer);
            if (layer != 0) continue;
            // owner pid must be one of ours
            CFNumberRef op = CFDictionaryGetValue(w, kCGWindowOwnerPID);
            if (!op) continue;
            int owner = -1;
            CFNumberGetValue(op, kCFNumberIntType, &owner);
            int ours = 0;
            for (int a = 1; a < argc && !ours; a++)
                if (atoi(argv[a]) == owner) ours = 1;
            if (!ours) continue;
            // non-empty bounds (same threshold as winwait)
            CFDictionaryRef b = CFDictionaryGetValue(w, kCGWindowBounds);
            if (!b) continue;
            CGRect r;
            if (!CGRectMakeWithDictionaryRepresentation(b, &r)) continue;
            if (r.size.width < 20 || r.size.height < 20) continue;
            count++;
        }
        CFRelease(list);
    }
    printf("%d\n", count);
    return 0;
}
