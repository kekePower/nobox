#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>

static int parse_signed(const char *value, int *result) {
    char *end = NULL;
    long parsed;

    errno = 0;
    parsed = strtol(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || parsed < INT_MIN || parsed > INT_MAX) {
        return 0;
    }
    *result = (int)parsed;
    return 1;
}

static int parse_dimension(const char *value, unsigned int *result) {
    char *end = NULL;
    unsigned long parsed;

    errno = 0;
    parsed = strtoul(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || parsed < 1 || parsed > 65535) {
        return 0;
    }
    *result = (unsigned int)parsed;
    return 1;
}

int main(int argc, char **argv) {
    char *end = NULL;
    unsigned long raw_window;
    int x;
    int y;
    unsigned int width;
    unsigned int height;
    Display *display;

    if (argc != 6) {
        fprintf(stderr, "usage: set-window-geometry WINDOW X Y WIDTH HEIGHT\n");
        return 2;
    }
    errno = 0;
    raw_window = strtoul(argv[1], &end, 0);
    if (errno != 0 || end == argv[1] || *end != '\0' ||
        !parse_signed(argv[2], &x) || !parse_signed(argv[3], &y) ||
        !parse_dimension(argv[4], &width) || !parse_dimension(argv[5], &height)) {
        fprintf(stderr, "invalid geometry\n");
        return 2;
    }
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    XMoveResizeWindow(display, (Window)raw_window, x, y, width, height);
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
