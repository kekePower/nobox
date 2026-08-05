#include <signal.h>
#include <stdint.h>
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
    Window root = DefaultRootWindow(display);
    Window window = XCreateSimpleWindow(display, root, 0, 0, 800, 600, 0, 0, 0x303030);
    Atom type_property = XInternAtom(display, "_NET_WM_WINDOW_TYPE", False);
    Atom desktop_type = XInternAtom(display, "_NET_WM_WINDOW_TYPE_DESKTOP", False);
    Atom expose_count = XInternAtom(display, "_NOBOX_TEST_DESKTOP_EXPOSES", False);
    XChangeProperty(display, window, type_property, XA_ATOM, 32, PropModeReplace,
                    (unsigned char *)&desktop_type, 1);
    XSelectInput(display, window, ExposureMask);
    XMapWindow(display, window);
    XFlush(display);
    printf("0x%lx\n", window);
    fflush(stdout);

    unsigned long count = 0;
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) {
        while (XPending(display) > 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == Expose) {
                ++count;
                XChangeProperty(display, root, expose_count, XA_CARDINAL, 32,
                                PropModeReplace, (unsigned char *)&count, 1);
                XFlush(display);
            }
        }
        usleep(1000);
    }
    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
