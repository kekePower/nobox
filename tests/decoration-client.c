#include <signal.h>
#include <stdlib.h>
#include <unistd.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;

    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 70, 70, 360, 120, 0, 0, 0xffffff);
    XSizeHints hints = {.flags = PPosition, .x = 70, .y = 70};
    XSetWMNormalHints(display, window, &hints);
    XStoreName(display, window, "nobox decoration regression");
    XMapWindow(display, window);
    XFlush(display);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
