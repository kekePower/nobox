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

static Window typed_window(Display *display, const char *type_name,
                           int x, int y, unsigned int width, unsigned int height,
                           const char *title) {
    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), x, y, width, height, 0, 0, 0x303030);
    Atom type_property = XInternAtom(display, "_NET_WM_WINDOW_TYPE", False);
    Atom window_type = XInternAtom(display, type_name, False);
    XChangeProperty(display, window, type_property, XA_ATOM, 32, PropModeReplace,
                    (unsigned char *)&window_type, 1);
    XStoreName(display, window, title);
    return window;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;

    Window desktop = typed_window(display, "_NET_WM_WINDOW_TYPE_DESKTOP",
                                  0, 0, 800, 600, "nobox desktop surface");
    Window dock = typed_window(display, "_NET_WM_WINDOW_TYPE_DOCK",
                               0, 0, 800, 30, "nobox desktop dock");
    Window first = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 60, 80, 280, 160, 0, 0, 0xffffff);
    Window second = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 420, 260, 280, 160, 0, 0, 0xe0e0e0);
    XStoreName(display, first, "nobox show desktop first");
    XStoreName(display, second, "nobox show desktop second");

    XMapWindow(display, desktop);
    XMapWindow(display, dock);
    XMapWindow(display, first);
    XMapWindow(display, second);
    XSync(display, False);
    printf("%#lx %#lx %#lx %#lx\n", desktop, dock, first, second);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, second);
    XDestroyWindow(display, first);
    XDestroyWindow(display, dock);
    XDestroyWindow(display, desktop);
    XCloseDisplay(display);
    return 0;
}
