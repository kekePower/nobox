#define _POSIX_C_SOURCE 200809L

#include <signal.h>
#include <stdio.h>
#include <time.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

static void settle(void) {
    const struct timespec delay = {.tv_sec = 0, .tv_nsec = 50000000L};
    nanosleep(&delay, NULL);
}

static Window create_window(Display *display, Window root, const char *title,
                            unsigned long color) {
    Window window = XCreateSimpleWindow(
        display, root, 0, 0, 140, 90, 0, 0, color);
    XStoreName(display, window, title);
    XClassHint class_hint = {
        .res_name = "nobox-xwayland-group",
        .res_class = "NoboxXWaylandGroup",
    };
    XSetClassHint(display, window, &class_hint);
    return window;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        return 2;
    }
    Window root = DefaultRootWindow(display);
    Window leader = XCreateSimpleWindow(display, root, 0, 0, 1, 1, 0, 0, 0);
    Window main = create_window(display, root, "nobox group main", 0x991111);
    Window helper = create_window(display, root, "nobox group transient", 0x119911);
    Window ordinary = create_window(display, root, "nobox group outsider", 0x111199);

    XWMHints hints = {
        .flags = WindowGroupHint,
        .window_group = leader,
    };
    XSetWMHints(display, main, &hints);
    XSetWMHints(display, helper, &hints);
    XSetTransientForHint(display, helper, root);

    /* Map the group transient first so policy must later reorder it above main. */
    XMapWindow(display, helper);
    XSync(display, False);
    settle();
    XMapWindow(display, main);
    XSync(display, False);
    settle();
    XMapWindow(display, ordinary);
    XSync(display, False);

    printf("main=0x%lx helper=0x%lx ordinary=0x%lx\n",
           main, helper, ordinary);
    fflush(stdout);
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) {
        const struct timespec delay = {.tv_sec = 0, .tv_nsec = 10000000L};
        nanosleep(&delay, NULL);
    }

    XDestroyWindow(display, ordinary);
    XDestroyWindow(display, helper);
    XDestroyWindow(display, main);
    XDestroyWindow(display, leader);
    XCloseDisplay(display);
    return 0;
}
