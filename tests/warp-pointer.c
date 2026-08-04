#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s WINDOW\n", argv[0]);
        return 2;
    }
    char *end = NULL;
    errno = 0;
    unsigned long raw_window = strtoul(argv[1], &end, 0);
    if (errno != 0 || end == argv[1] || *end != '\0') return 2;

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    XWarpPointer(display, None, (Window)raw_window, 0, 0, 0, 0, 20, 20);
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
