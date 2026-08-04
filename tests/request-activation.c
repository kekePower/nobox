#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

static Time server_time(Display *display) {
    Window window = XCreateSimpleWindow(display, DefaultRootWindow(display),
                                        -1, -1, 1, 1, 0, 0, 0);
    Atom marker = XInternAtom(display, "_NOBOX_TEST_TIMESTAMP", False);
    unsigned char value = 1;
    XSelectInput(display, window, PropertyChangeMask);
    XChangeProperty(display, window, marker, XA_INTEGER, 8, PropModeReplace,
                    &value, 1);
    XEvent event;
    XWindowEvent(display, window, PropertyChangeMask, &event);
    XDestroyWindow(display, window);
    return event.xproperty.time;
}

int main(int argc, char **argv) {
    char *end = NULL;
    unsigned long raw_window;
    Display *display;
    Atom active_window;
    XEvent event = {0};

    if (argc != 2 && argc != 4) {
        fprintf(stderr, "usage: request-activation WINDOW [SOURCE TIMESTAMP|current]\n");
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
    long source = 2;
    Time timestamp = CurrentTime;
    if (argc == 4) {
        char *source_end = NULL;
        source = strtol(argv[2], &source_end, 10);
        if (source_end == argv[2] || *source_end != '\0' || source < 0 || source > 2) {
            fprintf(stderr, "invalid activation source: %s\n", argv[2]);
            XCloseDisplay(display);
            return 2;
        }
        if (strcmp(argv[3], "current") == 0) {
            timestamp = server_time(display);
        } else {
            char *timestamp_end = NULL;
            unsigned long parsed_timestamp = strtoul(argv[3], &timestamp_end, 0);
            if (timestamp_end == argv[3] || *timestamp_end != '\0' ||
                parsed_timestamp > 0xffffffffUL) {
                fprintf(stderr, "invalid activation timestamp: %s\n", argv[3]);
                XCloseDisplay(display);
                return 2;
            }
            timestamp = (Time)parsed_timestamp;
        }
    }
    active_window = XInternAtom(display, "_NET_ACTIVE_WINDOW", False);
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = (Window)raw_window;
    event.xclient.message_type = active_window;
    event.xclient.format = 32;
    event.xclient.data.l[0] = source;
    event.xclient.data.l[1] = timestamp;
    XSendEvent(display, DefaultRootWindow(display), False,
               SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
