#include <stdio.h>
#include <unistd.h>
#include <X11/Xlib.h>

int main(void) {
    Display *display = XOpenDisplay(NULL);
    Window root;
    Window windows[3];

    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    root = DefaultRootWindow(display);
    for (int index = 0; index < 3; ++index) {
        windows[index] = XCreateSimpleWindow(
            display, root, 100, 100,
            (unsigned int)(311 + index), (unsigned int)(111 + index), 1,
            BlackPixel(display, 0), WhitePixel(display, 0));
        XMapWindow(display, windows[index]);
    }
    XFlush(display);
    sleep(10);
    XCloseDisplay(display);
    return 0;
}
