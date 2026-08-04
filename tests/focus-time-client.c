#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <X11/Xatom.h>
#include <X11/Xlib.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(int argc, char **argv) {
    if (argc != 4 || (strcmp(argv[1], "direct") != 0 && strcmp(argv[1], "indirect") != 0)) {
        fprintf(stderr, "usage: focus-time-client direct|indirect TIMESTAMP TITLE\n");
        return 2;
    }
    char *end = NULL;
    errno = 0;
    unsigned long parsed = strtoul(argv[2], &end, 0);
    if (errno != 0 || end == argv[2] || *end != '\0' || parsed > 0xffffffffUL) {
        fprintf(stderr, "invalid timestamp: %s\n", argv[2]);
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window root = DefaultRootWindow(display);
    Window window = XCreateSimpleWindow(display, root, 90, 90, 360, 120, 0, 0, 0xffffff);
    Window time_window = window;
    Atom user_time = XInternAtom(display, "_NET_WM_USER_TIME", False);
    Atom user_time_window = XInternAtom(display, "_NET_WM_USER_TIME_WINDOW", False);
    unsigned long timestamp = parsed;

    XStoreName(display, window, argv[3]);
    if (strcmp(argv[1], "indirect") == 0) {
        time_window = XCreateSimpleWindow(display, root, -1, -1, 1, 1, 0, 0, 0);
        XChangeProperty(display, window, user_time_window, XA_WINDOW, 32, PropModeReplace,
                        (const unsigned char *)&time_window, 1);
    }
    XChangeProperty(display, time_window, user_time, XA_CARDINAL, 32, PropModeReplace,
                    (const unsigned char *)&timestamp, 1);
    XMapWindow(display, window);
    XFlush(display);
    printf("0x%lx 0x%lx\n", window, time_window);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    if (time_window != window) XDestroyWindow(display, time_window);
    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
