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
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = (Window)raw_window;
    event.xclient.message_type = XInternAtom(display, "WM_CHANGE_STATE", False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = IconicState;
    XSendEvent(display, DefaultRootWindow(display), False,
               SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
