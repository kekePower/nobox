#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    char *end = NULL;
    if (argc != 3 || (strcmp(argv[2], "on") != 0 && strcmp(argv[2], "off") != 0)) {
        fprintf(stderr, "usage: %s WINDOW on|off\n", argv[0]);
        return 2;
    }
    errno = 0;
    unsigned long raw_window = strtoul(argv[1], &end, 0);
    if (errno != 0 || end == argv[1] || *end != '\0') return 2;
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 1;
    Window window = (Window)raw_window;
    XWMHints *existing = XGetWMHints(display, window);
    XWMHints hints = existing == NULL ? (XWMHints){0} : *existing;
    if (strcmp(argv[2], "on") == 0) {
        hints.flags |= XUrgencyHint;
    } else {
        hints.flags &= ~XUrgencyHint;
    }
    XSetWMHints(display, window, &hints);
    XFlush(display);
    if (existing != NULL) XFree(existing);
    XCloseDisplay(display);
    return 0;
}
