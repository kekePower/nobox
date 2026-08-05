#include <stdio.h>
#include <string.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

int main(int argc, char **argv) {
    int southeast = argc == 2 && strcmp(argv[1], "southeast") == 0;
    if (argc > 2 || (argc == 2 && !southeast)) {
        fprintf(stderr, "usage: %s [southeast]\n", argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open X display\n", stderr);
        return 2;
    }
    int screen = DefaultScreen(display);
    Window window = XCreateSimpleWindow(
        display, RootWindow(display, screen), 50, 60, 200, 120, 0,
        BlackPixel(display, screen), WhitePixel(display, screen));
    XSizeHints hints = {0};
    hints.flags = PPosition | PWinGravity;
    hints.x = 50;
    hints.y = 60;
    hints.win_gravity = southeast ? SouthEastGravity : NorthWestGravity;
    XSetWMNormalHints(display, window, &hints);
    XStoreName(display, window, southeast
        ? "nobox configure notify southeast"
        : "nobox configure notify normal");
    XSelectInput(display, window, StructureNotifyMask);
    XMapWindow(display, window);
    printf("window=0x%lx\n", window);
    fflush(stdout);

    for (;;) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type == ConfigureNotify) {
            XConfigureEvent configured = event.xconfigure;
            printf(
                "configure synthetic=%d event=0x%lx window=0x%lx "
                "x=%d y=%d width=%d height=%d border=%d above=0x%lx override=%d\n",
                configured.send_event, configured.event, configured.window,
                configured.x, configured.y, configured.width, configured.height,
                configured.border_width, configured.above,
                configured.override_redirect);
            fflush(stdout);
        }
        if (event.type == DestroyNotify) break;
    }
    XCloseDisplay(display);
    return 0;
}
