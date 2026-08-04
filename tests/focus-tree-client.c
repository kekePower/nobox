#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

static int coordinate(const char *value, int *result) {
    char *end = NULL;
    long parsed;

    errno = 0;
    parsed = strtol(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || parsed < 0 || parsed > 30000) {
        return 0;
    }
    *result = (int)parsed;
    return 1;
}

int main(int argc, char **argv) {
    Display *display;
    Window top;
    Window child;
    Atom child_atom;
    XSizeHints hints = {0};
    int x;
    int y;

    if (argc != 4 || !coordinate(argv[2], &x) || !coordinate(argv[3], &y)) {
        fprintf(stderr, "usage: focus-tree-client TITLE X Y\n");
        return 2;
    }
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }

    top = XCreateSimpleWindow(
        display, DefaultRootWindow(display), x, y, 280, 140, 0, 0, 0xffffff);
    child = XCreateSimpleWindow(display, top, 20, 20, 240, 100, 0, 0, 0x202020);
    hints.flags = PPosition;
    hints.x = x;
    hints.y = y;
    XSetWMNormalHints(display, top, &hints);
    XStoreName(display, top, argv[1]);
    child_atom = XInternAtom(display, "_NOBOX_TEST_FOCUS_CHILD", False);
    XChangeProperty(
        display,
        top,
        child_atom,
        XA_WINDOW,
        32,
        PropModeReplace,
        (unsigned char *)&child,
        1);
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
