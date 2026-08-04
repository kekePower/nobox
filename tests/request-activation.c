#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    char *end = NULL;
    unsigned long raw_window;
    Display *display;
    Atom active_window;
    XEvent event = {0};

    if (argc != 2) {
        fprintf(stderr, "usage: request-activation WINDOW\n");
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
    active_window = XInternAtom(display, "_NET_ACTIVE_WINDOW", False);
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = (Window)raw_window;
    event.xclient.message_type = active_window;
    event.xclient.format = 32;
    event.xclient.data.l[0] = 2;
    event.xclient.data.l[1] = CurrentTime;
    XSendEvent(display, DefaultRootWindow(display), False,
               SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
