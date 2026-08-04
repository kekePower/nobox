#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>

static void settle(void) {
    const struct timespec delay = {.tv_sec = 0, .tv_nsec = 100000000};
    nanosleep(&delay, NULL);
}

static int parse_offset(const char *source, int *result) {
    char *end = NULL;
    errno = 0;
    long value = strtol(source, &end, 10);
    if (errno != 0 || end == source || *end != '\0'
        || value < -32768 || value > 32767) return 0;
    *result = (int)value;
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 6) {
        fprintf(stderr,
                "usage: %s WINDOW move|resize commit|cancel DX DY\n",
                argv[0]);
        return 2;
    }
    unsigned int button = strcmp(argv[2], "move") == 0 ? Button1
        : strcmp(argv[2], "resize") == 0 ? Button3
        : 0;
    int cancel = strcmp(argv[3], "cancel") == 0;
    int dx;
    int dy;
    if (button == 0
        || (!cancel && strcmp(argv[3], "commit") != 0)
        || !parse_offset(argv[4], &dx)
        || !parse_offset(argv[5], &dy)) return 2;

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window window = (Window)strtoul(argv[1], NULL, 0);
    Window child;
    int root_x;
    int root_y;
    if (!XTranslateCoordinates(display, window, DefaultRootWindow(display),
                               10, 10, &root_x, &root_y, &child)) {
        XCloseDisplay(display);
        return 1;
    }
    KeyCode super = XKeysymToKeycode(display, XK_Super_L);
    KeyCode escape = XKeysymToKeycode(display, XK_Escape);
    if (super == 0 || escape == 0) {
        XCloseDisplay(display);
        return 1;
    }

    XTestFakeMotionEvent(display, DefaultScreen(display), root_x, root_y, 0);
    XTestFakeKeyEvent(display, super, True, 0);
    XTestFakeButtonEvent(display, button, True, 0);
    XFlush(display);
    settle();
    XTestFakeRelativeMotionEvent(display, dx, dy, 0);
    XFlush(display);
    settle();
    if (cancel) {
        XTestFakeKeyEvent(display, escape, True, 0);
        XTestFakeKeyEvent(display, escape, False, 0);
        XFlush(display);
        settle();
    }
    XTestFakeButtonEvent(display, button, False, 0);
    XTestFakeKeyEvent(display, super, False, 0);
    XSync(display, False);
    settle();
    XCloseDisplay(display);
    return 0;
}
