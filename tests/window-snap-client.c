#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <stdlib.h>
#include <unistd.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

static int parse_int(const char *source, int minimum, int maximum, int *result) {
    char *end = NULL;
    errno = 0;
    long value = strtol(source, &end, 10);
    if (errno != 0 || end == source || *end != '\0'
        || value < minimum || value > maximum) return 0;
    *result = (int)value;
    return 1;
}

int main(int argc, char **argv) {
    int x;
    int y;
    int width;
    int height;
    if (argc != 6
        || !parse_int(argv[2], INT_MIN, INT_MAX, &x)
        || !parse_int(argv[3], INT_MIN, INT_MAX, &y)
        || !parse_int(argv[4], 1, 65535, &width)
        || !parse_int(argv[5], 1, 65535, &height)) return 2;

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), x, y,
        (unsigned int)width, (unsigned int)height, 0, 0, 0xffffff);
    XSizeHints hints = {.flags = PPosition, .x = x, .y = y};
    XSetWMNormalHints(display, window, &hints);
    XStoreName(display, window, argv[1]);
    XMapWindow(display, window);
    XFlush(display);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
