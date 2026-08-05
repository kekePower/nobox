#define _POSIX_C_SOURCE 200809L

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int read_desktop(Display *display, Window window, Atom property,
                        unsigned long *desktop) {
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long count = 0;
    unsigned long remaining = 0;
    unsigned char *value = NULL;
    int status = XGetWindowProperty(display, window, property, 0, 1, False,
                                    XA_CARDINAL, &actual_type, &actual_format,
                                    &count, &remaining, &value);
    if (status != Success || actual_type != XA_CARDINAL || actual_format != 32 ||
        count != 1 || value == NULL) {
        if (value != NULL) XFree(value);
        return 0;
    }
    *desktop = *(unsigned long *)value;
    XFree(value);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 6) {
        fputs("usage: startup-client RESULT PID WID POINTER_X POINTER_Y\n", stderr);
        return 2;
    }
    const char *startup_id = getenv("DESKTOP_STARTUP_ID");
    if (startup_id == NULL || *startup_id == '\0') {
        fputs("DESKTOP_STARTUP_ID was not provided\n", stderr);
        return 1;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open X display\n", stderr);
        return 1;
    }
    int screen = DefaultScreen(display);
    Window window = XCreateSimpleWindow(display, RootWindow(display, screen),
                                        80, 90, 260, 120, 0,
                                        BlackPixel(display, screen),
                                        WhitePixel(display, screen));
    XClassHint class_hint = {
        .res_name = "nobox-startup",
        .res_class = "NoboxStartupTest",
    };
    XSetClassHint(display, window, &class_hint);
    XStoreName(display, window, "nobox startup notification");
    Atom utf8 = XInternAtom(display, "UTF8_STRING", False);
    Atom startup = XInternAtom(display, "_NET_STARTUP_ID", False);
    Atom desktop_property = XInternAtom(display, "_NET_WM_DESKTOP", False);
    XChangeProperty(display, window, startup, utf8, 8, PropModeReplace,
                    (const unsigned char *)startup_id,
                    (int)strlen(startup_id));
    XMapWindow(display, window);
    XFlush(display);

    unsigned long desktop = ~0UL;
    const struct timespec pause = {.tv_sec = 0, .tv_nsec = 50000000L};
    for (int attempt = 0; attempt < 60; ++attempt) {
        if (read_desktop(display, window, desktop_property, &desktop)) break;
        nanosleep(&pause, NULL);
    }
    FILE *result = fopen(argv[1], "w");
    if (result == NULL) {
        perror("could not create startup result");
        XDestroyWindow(display, window);
        XCloseDisplay(display);
        return 1;
    }
    fprintf(result, "window=%lu\nstartup_id=%s\ndesktop=%lu\n", window,
            startup_id, desktop);
    fprintf(result, "pid=%s\nwid=%s\npointer=%s %s\n", argv[2], argv[3],
            argv[4], argv[5]);
    if (fclose(result) != 0) {
        perror("could not finish startup result");
        XDestroyWindow(display, window);
        XCloseDisplay(display);
        return 1;
    }
    sleep(2);
    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return desktop == 1 ? 0 : 1;
}
