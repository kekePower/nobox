#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
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
    if (argc < 3 || argc > 4) {
        fprintf(stderr, "usage: %s TITLE normal|positioned|dialog [PARENT]\n", argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;

    int positioned = strcmp(argv[2], "positioned") == 0;
    int dialog = strcmp(argv[2], "dialog") == 0;
    if (!positioned && !dialog && strcmp(argv[2], "normal") != 0) return 2;
    if (dialog != (argc == 4)) return 2;

    unsigned int width = dialog ? 100U : 200U;
    unsigned int height = dialog ? 60U : 100U;
    int x = positioned ? 200 : 0;
    int y = positioned ? 200 : 0;
    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), x, y, width, height, 0, 0, 0xffffff);
    XStoreName(display, window, argv[1]);

    if (positioned) {
        XSizeHints hints = {.flags = PPosition, .x = x, .y = y};
        XSetWMNormalHints(display, window, &hints);
    }
    if (dialog) {
        Window parent = (Window)strtoul(argv[3], NULL, 0);
        XSetTransientForHint(display, window, parent);
        Atom property = XInternAtom(display, "_NET_WM_WINDOW_TYPE", False);
        Atom type = XInternAtom(display, "_NET_WM_WINDOW_TYPE_DIALOG", False);
        XChangeProperty(display, window, property, XA_ATOM, 32, PropModeReplace,
                        (const unsigned char *)&type, 1);
    }

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
