#define _POSIX_C_SOURCE 200809L

#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void settle(void) {
    const struct timespec pause = {.tv_sec = 0, .tv_nsec = 10000000L};
    nanosleep(&pause, NULL);
}

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open XWayland DISPLAY\n", stderr);
        return 2;
    }
    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 0, 0, 220, 120, 0, 0, 0x336699);
    XClassHint class_hint = {
        .res_name = "nobox-xwayland-activation",
        .res_class = "NoboxXWaylandActivation",
    };
    XSetClassHint(display, window, &class_hint);
    XStoreName(display, window, "nobox XWayland activation client");

    const char *startup_id = getenv("DESKTOP_STARTUP_ID");
    if (startup_id != NULL && startup_id[0] != '\0') {
        Atom property = XInternAtom(display, "_NET_STARTUP_ID", False);
        Atom utf8 = XInternAtom(display, "UTF8_STRING", False);
        XChangeProperty(display, window, property, utf8, 8, PropModeReplace,
                        (const unsigned char *)startup_id,
                        (int)strlen(startup_id));
    }
    XMapWindow(display, window);
    XSync(display, False);
    printf("pid=%ld window=0x%lx token=%s\n", (long)getpid(), window,
           startup_id == NULL ? "" : startup_id);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    int was_focused = -1;
    while (running) {
        Window focused = None;
        int revert_to = 0;
        XGetInputFocus(display, &focused, &revert_to);
        int is_focused = focused == window;
        if (is_focused != was_focused) {
            puts(is_focused ? "focus=activation" : "blur=activation");
            fflush(stdout);
            was_focused = is_focused;
        }
        settle();
    }
    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
