#include <stdio.h>
#include <unistd.h>
#include <X11/Xlib.h>

int main(void) {
    Display *display = XOpenDisplay(NULL);
    Window window;
    XEvent event = {0};

    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    window = XCreateSimpleWindow(display, DefaultRootWindow(display),
                                 60, 60, 410, 110, 1,
                                 BlackPixel(display, 0),
                                 WhitePixel(display, 0));
    XMapWindow(display, window);
    XFlush(display);
    sleep(1);

    event.xunmap.type = UnmapNotify;
    event.xunmap.display = display;
    event.xunmap.event = DefaultRootWindow(display);
    event.xunmap.window = window;
    event.xunmap.from_configure = False;
    XSendEvent(display, DefaultRootWindow(display), False,
               SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XFlush(display);
    sleep(5);

    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
