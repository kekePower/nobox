#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    char *end = NULL;
    unsigned long raw_window;
    Display *display;

    if (argc != 2) {
        fprintf(stderr, "usage: set-input-focus WINDOW\n");
        return 2;
    }
    errno = 0;
    raw_window = strtoul(argv[1], &end, 0);
    if (errno != 0 || end == argv[1] || *end != '\0') {
        fprintf(stderr, "invalid window identifier: %s\n", argv[1]);
        return 2;
    }
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    XSetInputFocus(display, (Window)raw_window, RevertToPointerRoot, CurrentTime);
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
