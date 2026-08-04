#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(int argc, char **argv) {
    if (argc != 2 || (strcmp(argv[1], "initial") != 0 && strcmp(argv[1], "normal") != 0)) {
        fprintf(stderr, "usage: %s initial|normal\n", argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 70, 70, 360, 120, 0, 0, 0xffffff);
    XSizeHints hints = {.flags = PPosition, .x = 70, .y = 70};
    XSetWMNormalHints(display, window, &hints);
    XStoreName(display, window, "nobox shade regression");
    if (strcmp(argv[1], "initial") == 0) {
        Atom state = XInternAtom(display, "_NET_WM_STATE", False);
        Atom shaded = XInternAtom(display, "_NET_WM_STATE_SHADED", False);
        XChangeProperty(display, window, state, XA_ATOM, 32, PropModeReplace,
                        (unsigned char *)&shaded, 1);
    }
    XMapWindow(display, window);
    XSync(display, False);
    printf("%#lx\n", window);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();
    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
