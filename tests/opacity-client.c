#include <signal.h>
#include <stdio.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;

    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 90, 90, 320, 160, 0, 0, 0xffffff);
    XStoreName(display, window, "nobox opacity client");
    Atom opacity = XInternAtom(display, "_NET_WM_WINDOW_OPACITY", False);
    unsigned long initial = 0x7fffffffUL;
    XChangeProperty(display, window, opacity, XA_CARDINAL, 32, PropModeReplace,
                    (const unsigned char *)&initial, 1);
    XMapWindow(display, window);
    XFlush(display);

    printf("0x%lx\n", window);
    fflush(stdout);
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
