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
        display, DefaultRootWindow(display), 0, 0, 800, 30, 0, 0, 0x333333);
    Atom type_property = XInternAtom(display, "_NET_WM_WINDOW_TYPE", False);
    Atom dock_type = XInternAtom(display, "_NET_WM_WINDOW_TYPE_DOCK", False);
    Atom partial = XInternAtom(display, "_NET_WM_STRUT_PARTIAL", False);
    unsigned long strut[12] = {0, 0, 30, 0, 0, 0, 0, 0, 0, 799, 0, 0};
    XChangeProperty(display, window, type_property, XA_ATOM, 32, PropModeReplace,
                    (unsigned char *)&dock_type, 1);
    XChangeProperty(display, window, partial, XA_CARDINAL, 32, PropModeReplace,
                    (unsigned char *)strut, 12);
    XStoreName(display, window, "nobox strut dock");
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
