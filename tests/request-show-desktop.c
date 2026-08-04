#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s 0|1\n", argv[0]);
        return 2;
    }
    char *end = NULL;
    errno = 0;
    unsigned long raw_state = strtoul(argv[1], &end, 0);
    if (errno != 0 || end == argv[1] || *end != '\0' || raw_state > UINT32_MAX) {
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window root = DefaultRootWindow(display);
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = root;
    event.xclient.message_type = XInternAtom(display, "_NET_SHOWING_DESKTOP", False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = (long)raw_state;
    XSendEvent(display, root, False, SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
