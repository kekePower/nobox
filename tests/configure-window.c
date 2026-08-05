#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xlib.h>

static int parse_unsigned(const char *text, unsigned long maximum, unsigned long *value) {
    char *end = NULL;
    errno = 0;
    *value = strtoul(text, &end, 0);
    return errno == 0 && end != text && *end == '\0' && *value <= maximum;
}

static int parse_coordinate(const char *text, int *value) {
    char *end = NULL;
    errno = 0;
    long parsed = strtol(text, &end, 0);
    if (errno != 0 || end == text || *end != '\0' || parsed < INT_MIN || parsed > INT_MAX) {
        return 0;
    }
    *value = (int)parsed;
    return 1;
}

int main(int argc, char **argv) {
    unsigned long raw_window;
    if (argc != 5 || !parse_unsigned(argv[2], ULONG_MAX, &raw_window) ||
        (strcmp(argv[1], "move") != 0 && strcmp(argv[1], "resize") != 0)) {
        fprintf(stderr, "usage: %s move WINDOW X Y | resize WINDOW WIDTH HEIGHT\n", argv[0]);
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open X display\n", stderr);
        return 1;
    }
    if (strcmp(argv[1], "move") == 0) {
        int x;
        int y;
        if (!parse_coordinate(argv[3], &x) || !parse_coordinate(argv[4], &y)) {
            fputs("coordinates must fit in a signed integer\n", stderr);
            XCloseDisplay(display);
            return 2;
        }
        XMoveWindow(display, (Window)raw_window, x, y);
    } else {
        unsigned long width;
        unsigned long height;
        if (!parse_unsigned(argv[3], UINT_MAX, &width) ||
            !parse_unsigned(argv[4], UINT_MAX, &height) || width == 0 || height == 0) {
            fputs("width and height must be positive X11 dimensions\n", stderr);
            XCloseDisplay(display);
            return 2;
        }
        XResizeWindow(display, (Window)raw_window, (unsigned int)width, (unsigned int)height);
    }
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
