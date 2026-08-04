#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s WINDOW add|remove|toggle\n", argv[0]);
        return 2;
    }
    long action = strcmp(argv[2], "remove") == 0 ? 0
        : strcmp(argv[2], "add") == 0 ? 1
        : strcmp(argv[2], "toggle") == 0 ? 2
        : -1;
    if (action < 0) return 2;

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window window = (Window)strtoul(argv[1], NULL, 0);
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = window;
    event.xclient.message_type = XInternAtom(display, "_NET_WM_STATE", False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = action;
    event.xclient.data.l[1] = XInternAtom(
        display, "_NET_WM_STATE_MAXIMIZED_HORZ", False);
    event.xclient.data.l[2] = XInternAtom(
        display, "_NET_WM_STATE_MAXIMIZED_VERT", False);
    event.xclient.data.l[3] = 1;
    XSendEvent(display, DefaultRootWindow(display), False,
               SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
