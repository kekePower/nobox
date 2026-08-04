#include <signal.h>
#include <stdio.h>
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

    Window root = DefaultRootWindow(display);
    XSetWindowAttributes override_attributes = {
        .background_pixel = 0x405060,
        .override_redirect = True,
    };
    Window override_window = XCreateWindow(
        display, root, 500, 40, 120, 80, 0, CopyFromParent, InputOutput,
        CopyFromParent, CWBackPixel | CWOverrideRedirect, &override_attributes);
    XStoreName(display, override_window, "nobox override regression");

    XSetWindowAttributes input_attributes = {.override_redirect = True};
    Window input_window = XCreateWindow(
        display, root, 500, 140, 120, 80, 0, 0, InputOnly, CopyFromParent,
        CWOverrideRedirect, &input_attributes);

    Window parent = XCreateSimpleWindow(display, root, 60, 70, 320, 180, 7, 0, 0xffffff);
    Window child = XCreateSimpleWindow(display, root, 120, 130, 180, 90, 9, 0, 0x202020);
    XStoreName(display, parent, "nobox modal parent regression");
    XStoreName(display, child, "nobox modal child regression");
    XSetTransientForHint(display, child, parent);

    XMapWindow(display, override_window);
    XMapWindow(display, input_window);
    XMapWindow(display, parent);
    XMapWindow(display, child);
    XSync(display, False);

    printf("%#lx %#lx %#lx %#lx\n", override_window, input_window, parent, child);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, child);
    XDestroyWindow(display, parent);
    XDestroyWindow(display, input_window);
    XDestroyWindow(display, override_window);
    XCloseDisplay(display);
    return 0;
}
