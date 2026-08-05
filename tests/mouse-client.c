#include <stdio.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: mouse-client TITLE\n");
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    Window root = DefaultRootWindow(display);
    Window window = XCreateSimpleWindow(display, root, 120, 260, 320, 140,
                                        0, 0, 0xffffff);
    XStoreName(display, window, argv[1]);
    XSelectInput(display, window, ButtonPressMask);
    XMapWindow(display, window);
    XFlush(display);

    Atom pressed = XInternAtom(display, "_NOBOX_TEST_BUTTON_PRESS", False);
    for (;;) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type == ButtonPress) {
            unsigned long value = window;
            XChangeProperty(display, root, pressed, XA_WINDOW, 32,
                            PropModeReplace, (unsigned char *)&value, 1);
            XFlush(display);
        }
    }
}
