#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc != 6) {
        fprintf(stderr, "usage: request-fullscreen-monitors WINDOW TOP BOTTOM LEFT RIGHT\n");
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open X display\n", stderr);
        return 1;
    }

    char *end = NULL;
    Window window = strtoul(argv[1], &end, 0);
    if (end == argv[1] || *end != '\0') {
        fputs("invalid window id\n", stderr);
        XCloseDisplay(display);
        return 2;
    }

    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.window = window;
    event.xclient.message_type = XInternAtom(display, "_NET_WM_FULLSCREEN_MONITORS", False);
    event.xclient.format = 32;
    for (int index = 0; index < 4; ++index) {
        end = NULL;
        event.xclient.data.l[index] = strtol(argv[index + 2], &end, 10);
        if (end == argv[index + 2] || *end != '\0') {
            fputs("invalid monitor index\n", stderr);
            XCloseDisplay(display);
            return 2;
        }
    }
    event.xclient.data.l[4] = 1;

    int sent = XSendEvent(display, DefaultRootWindow(display), False,
                          SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XSync(display, False);
    XCloseDisplay(display);
    return sent == 0;
}
