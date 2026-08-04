#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    char *end = NULL;
    if (argc != 2) {
        fprintf(stderr, "usage: %s WINDOW\n", argv[0]);
        return 2;
    }
    errno = 0;
    unsigned long raw_window = strtoul(argv[1], &end, 0);
    if (errno != 0 || end == argv[1] || *end != '\0') return 2;
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 1;
    Window root;
    int x, y;
    unsigned int width, height, border, depth;
    Window window = (Window)raw_window;
    if (!XGetGeometry(display, window, &root, &x, &y, &width, &height, &border, &depth)) {
        XCloseDisplay(display);
        return 1;
    }
    XSizeHints hints = {0};
    hints.flags = PMinSize | PMaxSize;
    hints.min_width = hints.max_width = (int)width;
    hints.min_height = hints.max_height = (int)height;
    XSetWMNormalHints(display, window, &hints);
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
