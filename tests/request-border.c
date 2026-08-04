#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s WINDOW WIDTH\n", argv[0]);
        return 2;
    }

    char *window_end = NULL;
    char *width_end = NULL;
    errno = 0;
    unsigned long raw_window = strtoul(argv[1], &window_end, 0);
    unsigned long raw_width = strtoul(argv[2], &width_end, 0);
    if (errno != 0 || window_end == argv[1] || *window_end != '\0' ||
        width_end == argv[2] || *width_end != '\0' || raw_width > INT_MAX) {
        fprintf(stderr, "invalid window or border width\n");
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    XSetWindowBorderWidth(display, (Window)raw_window, (unsigned int)raw_width);
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
