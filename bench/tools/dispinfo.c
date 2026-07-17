// dispinfo — print "X Y W H" (global desktop coordinates) of the display
// containing the mouse cursor, i.e. the user's CURRENT display.
//
// Used by the fullscreen measurement mode to pin every terminal's window to
// the display the user launched the harness from — without pinning, macOS
// places windows on whatever display an app last used, so measured windows
// (and their fullscreen Spaces) could land on another monitor.
//
// Reading cursor location + display bounds needs no special permission.
// Falls back to the main display if the cursor is somehow on none.
#include <ApplicationServices/ApplicationServices.h>
#include <stdio.h>

int main(void) {
    CGEventRef e = CGEventCreate(NULL);
    CGPoint p = CGEventGetLocation(e);
    CFRelease(e);

    CGDirectDisplayID ids[16];
    uint32_t n = 0;
    CGGetActiveDisplayList(16, ids, &n);
    for (uint32_t i = 0; i < n; i++) {
        CGRect b = CGDisplayBounds(ids[i]);
        if (CGRectContainsPoint(b, p)) {
            printf("%d %d %d %d\n", (int)b.origin.x, (int)b.origin.y,
                   (int)b.size.width, (int)b.size.height);
            return 0;
        }
    }
    CGRect b = CGDisplayBounds(CGMainDisplayID());
    printf("%d %d %d %d\n", (int)b.origin.x, (int)b.origin.y,
           (int)b.size.width, (int)b.size.height);
    return 0;
}
