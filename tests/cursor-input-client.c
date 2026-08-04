#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/cursorfont.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    Window root;
    Window top;
    Window child;
    XSetWindowAttributes attributes = {0};
    XSizeHints hints = {0};

    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    root = DefaultRootWindow(display);
    top = XCreateSimpleWindow(display, root, 120, 100, 400, 180, 0, 0, 0xffffff);
    attributes.cursor = XCreateFontCursor(display, XC_watch);
    child = XCreateWindow(
        display,
        top,
        80,
        45,
        240,
        90,
        0,
        0,
        InputOnly,
        CopyFromParent,
        CWCursor,
        &attributes);
    hints.flags = PPosition;
    hints.x = 120;
    hints.y = 100;
    XSetWMNormalHints(display, top, &hints);
    XStoreName(display, top, "nobox-input-only-cursor");
    XMapWindow(display, child);
    XMapWindow(display, top);
    XFlush(display);

    printf("0x%lx 0x%lx\n", top, child);
    fflush(stdout);
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, top);
    XCloseDisplay(display);
    return 0;
}
