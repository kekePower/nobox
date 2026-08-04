#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    char *end = NULL;
    unsigned long raw_window;
    Display *display;
    XEvent event = {0};

    if (argc != 2) {
        fprintf(stderr, "usage: click-window WINDOW\n");
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

    event.xbutton.type = ButtonPress;
    event.xbutton.display = display;
    event.xbutton.window = (Window)raw_window;
    event.xbutton.root = DefaultRootWindow(display);
    event.xbutton.subwindow = None;
    event.xbutton.button = Button1;
    event.xbutton.same_screen = True;
    XSendEvent(display, (Window)raw_window, False, ButtonPressMask, &event);
    event.xbutton.type = ButtonRelease;
    XSendEvent(display, (Window)raw_window, False, ButtonReleaseMask, &event);
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
